//! Verification codes (docs/verification-codes.md): a code its sender signed is kept for autofill
//! and skips greylisting, within the limit per sender; a header nobody signed is only a guess.

use uwumail_smtp::SpamConfig;
use uwumail_store::verification::Source;
use uwumail_store::{DkimKey, VERIFICATION_LIMIT_PER_HOUR};

use crate::flow::{RawSession, TestServer, spam_test_server};

/// Every judged message is suspicious, so every stranger waits once for the greylist.
async fn greylisting_server() -> TestServer {
    let spam = SpamConfig { greylist_score: -100.0, junk_score: 100.0, ..SpamConfig::default() };
    spam_test_server(spam, Some("v=DMARC1; p=none")).await
}

/// Hands in `message` the way the relay received it from 203.0.113.7.
async fn relay(server: &TestServer, from: &str, helo: &str, message: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command(&format!("MAIL FROM:<{from}>")).await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    session
        .command(&format!(
            "Received: from {helo} ({helo} [203.0.113.7])\r\n\tby relay.local (Postfix) with ESMTPS id 4F2;\r\n\t{date}\r\n\
             {message}.",
            date = mail_builder::headers::date::Date::now().to_rfc822(),
        ))
        .await
}

/// A code mail of sender.test with the header, signed with `keys` when there are any.
fn code_mail(domain: &str, number: usize, keys: &[DkimKey]) -> String {
    let message = format!(
        "From: Shop <no-reply@{domain}>\r\nTo: mini@a.test\r\nSubject: Dein Code\r\nDate: {date}\r\n\
         Message-ID: <code-{number}@{domain}>\r\n\
         Verification-Info: kind=otp; origin=login.{domain}; code=77711{number}; purpose=login;\r\n \
         expires=2099-01-01T00:00:00Z\r\n\r\nDein Code: 48291{number}\r\n",
        date = mail_builder::headers::date::Date::now().to_rfc822(),
    );
    let signature =
        if keys.is_empty() { String::new() } else { uwumail_smtp::dkim::sign(message.as_bytes(), keys).unwrap() };
    format!("{signature}{message}")
}

async fn codes(server: &TestServer) -> Vec<Option<uwumail_store::verification::Detected>> {
    let store = server.smtp.store();
    let account = store.account("mini@a.test").await.unwrap().unwrap().id;
    let ids: Vec<i64> = server.inbox("mini@a.test").await.iter().map(|email| email.id).collect();
    let mut found = store.verifications(account, ids.clone()).await.unwrap();
    ids.iter().map(|id| found.remove(id)).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_codes_skip_the_greylist_within_the_limit() {
    let a = greylisting_server().await;
    // sender.test is a stranger here: its keys live in a store of its own.
    let dir = tempfile::tempdir().unwrap();
    let sender = uwumail_store::Store::open(dir.path()).await.unwrap();
    sender.create_domain("sender.test").await.unwrap();
    let keys = uwumail_smtp::dkim::ensure_domain_keys(&sender, "sender.test").await.unwrap();
    for key in &keys {
        let (name, value) = key.dns_record();
        a.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }

    let limit = VERIFICATION_LIMIT_PER_HOUR as usize;
    for number in 0..limit {
        let reply =
            relay(&a, "no-reply@sender.test", "mail.sender.test", &code_mail("sender.test", number, &keys)).await;
        assert!(reply.starts_with("250"), "a signed code does not wait: {reply}");
    }
    let found = codes(&a).await;
    assert_eq!(found.len(), limit);
    let first = found.iter().flatten().find(|found| found.code.as_deref() == Some("777110")).unwrap();
    assert_eq!((first.source, first.autofill, first.origin.as_str()), (Source::Header, true, "login.sender.test"));
    assert!(found.iter().all(|found| found.as_ref().is_some_and(|found| found.source == Source::Header)));

    // One more within the hour is a normal mail: it waits like any other and has no code.
    let over = code_mail("sender.test", limit, &keys);
    let reply = relay(&a, "no-reply@sender.test", "mail.sender.test", &over).await;
    assert!(reply.starts_with("451 4.7.1"), "past the limit there is no fast lane: {reply}");
    let reply = relay(&a, "no-reply@sender.test", "mail.sender.test", &over).await;
    assert!(reply.starts_with("250"), "{reply}");
    let found = codes(&a).await;
    assert_eq!((found.len(), found.iter().flatten().count()), (limit + 1, limit));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_header_without_a_signature_over_it_is_only_a_guess() {
    let a = greylisting_server().await;
    // SPF passes aligned, so DMARC passes; but SPF vouches for no header.
    a.smtp.dns_cache().pin_txt("other.test", "v=spf1 ip4:203.0.113.7 -all").unwrap();
    a.smtp.dns_cache().pin_no_txt("mail.other.test");
    a.smtp.dns_cache().pin_txt("_dmarc.other.test", "v=DMARC1; p=none").unwrap();

    let message = code_mail("other.test", 1, &[]);
    let reply = relay(&a, "no-reply@other.test", "mail.other.test", &message).await;
    assert!(reply.starts_with("451 4.7.1"), "a guess gets no fast lane: {reply}");
    let reply = relay(&a, "no-reply@other.test", "mail.other.test", &message).await;
    assert!(reply.starts_with("250"), "{reply}");

    let found = codes(&a).await;
    let found = found[0].as_ref().expect("the code in the text");
    assert_eq!(
        (found.source, found.code.as_deref(), found.origin.as_str(), found.autofill),
        (Source::Heuristic, Some("482911"), "other.test", true)
    );
}
