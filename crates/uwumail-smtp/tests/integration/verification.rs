//! Verification codes (docs/verification-codes.md): a code its sender signed is kept for autofill
//! and skips greylisting, within the limits per sender and per account; a header nobody signed is
//! only a guess; and nobody here can send one that our own signature would vouch for.

use uwumail_smtp::SpamConfig;
use uwumail_store::verification::Source;
use uwumail_store::{DkimKey, VERIFICATION_FAST_LANE_PER_HOUR, VERIFICATION_LIMIT_PER_HOUR};

use crate::flow::{RawSession, TestServer, spam_test_server_for, start};

/// Every judged message is suspicious, so every stranger waits once for the greylist.
async fn greylisting_server() -> TestServer {
    let spam = SpamConfig { greylist_score: -100.0, junk_score: 100.0, ..SpamConfig::default() };
    spam_test_server_for(&["mini", "ami"], spam, Some("v=DMARC1; p=none")).await
}

/// Hands in `message` for mini@a.test the way the relay received it from 203.0.113.7.
async fn relay(server: &TestServer, from: &str, helo: &str, message: &str) -> String {
    relay_to(server, &["mini@a.test"], from, helo, message).await
}

async fn relay_to(server: &TestServer, recipients: &[&str], from: &str, helo: &str, message: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command(&format!("MAIL FROM:<{from}>")).await.starts_with("250"));
    for recipient in recipients {
        assert!(session.command(&format!("RCPT TO:<{recipient}>")).await.starts_with("250"));
    }
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

/// The DKIM keys of sender.test, published to `server`. sender.test is a stranger there: its keys
/// live in a store of its own.
async fn sender_keys(server: &TestServer) -> (Vec<DkimKey>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let sender = uwumail_store::Store::open(dir.path()).await.unwrap();
    sender.create_domain("sender.test").await.unwrap();
    let keys = uwumail_smtp::dkim::ensure_domain_keys(&sender, "sender.test").await.unwrap();
    for key in &keys {
        let (name, value) = key.dns_record();
        server.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }
    (keys, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_codes_skip_the_greylist_within_the_limit() {
    let a = greylisting_server().await;
    let (keys, _dir) = sender_keys(&a).await;

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

#[tokio::test(flavor = "multi_thread")]
async fn the_fast_lane_lifts_greylisting_only_for_its_own_recipients() {
    let a = greylisting_server().await;
    let (keys, _dir) = sender_keys(&a).await;
    // ami already had the hour's share of signed codes, from many senders (VC-02).
    let store = a.smtp.store();
    let ami = store.account("ami@a.test").await.unwrap().unwrap().id;
    for n in 0..VERIFICATION_FAST_LANE_PER_HOUR {
        assert!(store.take_verification_fast_lane(ami, &format!("sender{n}.example")).await.unwrap());
    }
    assert!(!store.take_verification_fast_lane(ami, "sender.test").await.unwrap());

    // mini may still have one, but that does not carry the mail past greylisting for ami.
    let message = code_mail("sender.test", 1, &keys);
    let both = ["mini@a.test", "ami@a.test"];
    let reply = relay_to(&a, &both, "no-reply@sender.test", "mail.sender.test", &message).await;
    assert!(reply.starts_with("451 4.7.1"), "{reply}");
    // To mini alone it goes through at once.
    let reply = relay(&a, "no-reply@sender.test", "mail.sender.test", &code_mail("sender.test", 2, &keys)).await;
    assert!(reply.starts_with("250"), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_sender_cannot_send_a_verification_info_header() {
    use uwumail_smtp::{Submission, SubmissionRecipient};
    let a = start("a.test", &["mini", "ami"], &[]).await;
    let account = a.smtp.store().account("mini@a.test").await.unwrap().unwrap();
    // Our DKIM signature covers the header: kept, it would vouch for any code mini wrote (VC-01).
    let raw = "From: mini@a.test\r\nTo: ami@a.test\r\nSubject: Dein Code\r\n\
               Verification-Info: kind=otp; origin=login.a.test; code=777111;\r\n expires=2099-01-01T00:00:00Z\r\n\
               \r\nHallo\r\n";
    let submission = Submission {
        account,
        mail_from: "mini@a.test".into(),
        recipients: vec![SubmissionRecipient::new("ami@a.test")],
        raw: raw.as_bytes().to_vec(),
        env_id: None,
        trace: None,
    };
    a.smtp.submit(submission).await.unwrap();
    let inbox = a.wait_for_inbox("ami@a.test", 1).await;
    let delivered = a.raw(&inbox[0]).await;
    // Only the DKIM signature names it, oversigned: as absent.
    let lines = delivered.to_ascii_lowercase();
    assert!(!lines.lines().any(|line| line.starts_with("verification-info:")), "{delivered}");
    assert!(!delivered.contains("777111"), "{delivered}");
}

/// An app on this server mails a code to a local account through submission (0.30.0 regression:
/// such mail never went through detection).
#[tokio::test(flavor = "multi_thread")]
async fn a_code_from_an_app_on_this_server_is_found() {
    use uwumail_smtp::{Submission, SubmissionRecipient};
    let a = start("a.test", &["mini", "ami"], &[]).await;
    let account = a.smtp.store().account("mini@a.test").await.unwrap().unwrap();
    let raw = "From: Lock <mini@a.test>\r\nTo: ami@a.test\r\nSubject: Anmeldung\r\n\r\n\
               Dein Anmeldecode: 133900\r\nEr gilt 10 Minuten.\r\n";
    let submission = Submission {
        account,
        mail_from: "mini@a.test".into(),
        recipients: vec![SubmissionRecipient::new("ami@a.test")],
        raw: raw.as_bytes().to_vec(),
        env_id: None,
        trace: None,
    };
    a.smtp.submit(submission).await.unwrap();
    let inbox = a.wait_for_inbox("ami@a.test", 1).await;
    let store = a.smtp.store();
    let ami = store.account("ami@a.test").await.unwrap().unwrap().id;
    let found = store.verifications(ami, vec![inbox[0].id]).await.unwrap().remove(&inbox[0].id).expect("a code");
    assert_eq!(found.code.as_deref(), Some("133900"));
    assert_eq!(found.origin, "a.test");
    assert!(found.autofill);
}
