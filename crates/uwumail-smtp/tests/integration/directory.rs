//! Groups, shared mailboxes and masked addresses (docs/groups.md, docs/jmap-masked-email.md), end
//! to end through the SMTP doors.

use lettre::AsyncTransport;

use uwumail_store::{
    DomainKind, DomainMaskedPolicy, GroupUpdate, IngestRequest, ListScope, MailboxRole, MailboxTarget, MaskedMode,
    MaskedState, MaskedUpdate, NewAccount, NewGroup, NewMaskedAddress, NewSenderListEntry, NewSharedMailbox, Role,
    SenderList, WhoMaySend,
};

use crate::flow::{PASSWORD, RawSession, TestServer, mail, start};

async fn server(users: &[&str]) -> TestServer {
    let a = start("a.test", users, &[]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    a
}

/// Hands in a message from news@sender.test for `recipients`; returns the answer to the data, or
/// the first refused RCPT.
async fn from_outside(server: &TestServer, recipients: &[&str], headers: &str, subject: &str) -> String {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<news@sender.test>").await.starts_with("250"));
    for recipient in recipients {
        let reply = session.command(&format!("RCPT TO:<{recipient}>")).await;
        if !reply.starts_with("250") {
            return reply;
        }
    }
    assert!(session.command("DATA").await.starts_with("354"));
    session.command(&format!("{headers}From: news@sender.test\r\nSubject: {subject}\r\n\r\nHallo\r\n.")).await
}

fn group(address: &str, members: &[&str], who_may_send: WhoMaySend) -> NewGroup {
    NewGroup {
        address: address.into(),
        name: "Vorstand".into(),
        who_may_send,
        members_may_send_as: false,
        members: members.iter().map(|member| (*member).to_owned()).collect(),
    }
}

/// The messages in a folder of an account, by name.
async fn folder(server: &TestServer, login: &str, name: &str) -> Vec<uwumail_store::EmailSummary> {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap();
    let Some(mailbox) = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.name == name) else {
        return Vec::new();
    };
    store.emails_in_mailbox(mailbox.id, 50).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_delivers_to_each_member_on_their_own() {
    let a = server(&["mini", "leni"]).await;
    let store = a.smtp.store().clone();
    // Nyu's mailbox is full; that is her loss alone.
    store
        .create_account(NewAccount {
            address: "nyu@a.test".into(),
            display_name: "Nyu".into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 10,
            protocols: None,
        })
        .await
        .unwrap();
    store
        .create_group(group("vorstand@a.test", &["mini@a.test", "leni@a.test", "nyu@a.test"], WhoMaySend::Anyone))
        .await
        .unwrap();
    // Leni blocked the sender for herself: her copy goes to Junk, the others' do not.
    let leni = store.account("leni@a.test").await.unwrap().unwrap();
    store
        .add_sender_list_entry(NewSenderListEntry {
            scope: ListScope::Account(leni.id),
            list: SenderList::Block,
            kind: None,
            value: "news@sender.test".into(),
            note: String::new(),
            created_by: String::new(),
            expires_at: None,
        })
        .await
        .unwrap();

    // Mini sorts the board's mail into a folder of her own with her rules.
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let script = br#"require ["fileinto", "mailbox"];
if header :contains "subject" "Sitzung" { fileinto :create "Vorstand"; }
"#;
    uwumail_smtp::sieve::validate(script).unwrap();
    let created = store.create_sieve_script(mini, Some("Vorstand"), script).await.unwrap();
    store.activate_sieve_script(mini, Some(created.id)).await.unwrap();

    // Mini is reached directly and through the group, and gets it once. The answer comes after
    // every member was delivered to, so there is nothing to wait for.
    let reply = from_outside(&a, &["vorstand+sitzung@a.test", "mini@a.test"], "", "Sitzung").await;
    assert!(reply.starts_with("250"), "{reply}");
    let filed = folder(&a, "mini@a.test", "Vorstand").await;
    assert_eq!(filed.iter().map(|email| email.subject.as_str()).collect::<Vec<_>>(), ["Sitzung"], "no second copy");
    assert!(a.inbox("mini@a.test").await.is_empty());
    assert_eq!(a.mailbox("leni@a.test", MailboxRole::Junk).await.len(), 1);
    assert!(a.inbox("leni@a.test").await.is_empty());
    assert!(a.inbox("nyu@a.test").await.is_empty());

    // A group with every mailbox full fails like a person would.
    store
        .update_group("vorstand@a.test", GroupUpdate { members: Some(vec!["nyu@a.test".into()]), ..Default::default() })
        .await
        .unwrap();
    let reply = from_outside(&a, &["vorstand@a.test"], "", "Voll").await;
    assert!(reply.starts_with("552 5.2.2"), "{reply}");

    // A message that went through the group before goes round no more.
    store
        .update_group(
            "vorstand@a.test",
            GroupUpdate { members: Some(vec!["mini@a.test".into()]), ..Default::default() },
        )
        .await
        .unwrap();
    let reply = from_outside(&a, &["vorstand@a.test"], "Delivered-To: vorstand@a.test\r\n", "Schleife").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert!(a.inbox("mini@a.test").await.is_empty(), "the loop was not delivered");
    let reply = from_outside(&a, &["vorstand@a.test"], "", "Protokoll").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);

    // Members of an empty group: nobody takes the mail.
    store.update_group("vorstand@a.test", GroupUpdate { members: Some(vec![]), ..Default::default() }).await.unwrap();
    let reply = from_outside(&a, &["vorstand@a.test"], "", "Leer").await;
    assert!(reply.starts_with("550 5.1.1"), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn who_may_write_to_a_group_is_checked_at_the_door() {
    let a = server(&["mini", "leni", "ami"]).await;
    let store = a.smtp.store().clone();
    let members = group("vorstand@a.test", &["mini@a.test", "leni@a.test"], WhoMaySend::Members);
    store.create_group(NewGroup { members_may_send_as: true, ..members }).await.unwrap();

    let reply = from_outside(&a, &["vorstand@a.test"], "", "Werbung").await;
    assert!(reply.starts_with("550 5.7.1") && reply.contains("Only members"), "{reply}");

    // Claiming a member's address from elsewhere is not enough: nothing vouches for the sender.
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO client.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<vorstand@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command("From: mini@a.test\r\nSubject: Gefälscht\r\n\r\nHallo\r\n.").await;
    assert!(reply.starts_with("550 5.7.1"), "{reply}");
    assert!(a.inbox("leni@a.test").await.is_empty());

    // A member writes from their mail app, and may use the group's address.
    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("Mini <mini@a.test>", &["vorstand@a.test"], "Protokoll"))
        .await
        .unwrap();
    assert_eq!(a.inbox("leni@a.test").await[0].subject, "Protokoll");
    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("Vorstand <vorstand@a.test>", &["ami@a.test"], "Einladung"))
        .await
        .unwrap();
    assert_eq!(a.inbox("ami@a.test").await[0].subject, "Einladung");

    // Someone else of the domain may neither write to it nor send as it.
    let refused = a.mailer("ami@a.test", PASSWORD, false).send(mail("ami@a.test", &["vorstand@a.test"], "Frage")).await;
    assert!(refused.is_err());
    let refused =
        a.mailer("ami@a.test", PASSWORD, false).send(mail("vorstand@a.test", &["leni@a.test"], "Falsch")).await;
    assert!(refused.is_err());

    // Only the domain: everyone here, nobody from elsewhere.
    store
        .update_group("vorstand@a.test", GroupUpdate { who_may_send: Some(WhoMaySend::Domain), ..Default::default() })
        .await
        .unwrap();
    let reply = from_outside(&a, &["vorstand@a.test"], "", "Werbung").await;
    assert!(reply.starts_with("550 5.7.1") && reply.contains("a.test"), "{reply}");
    a.mailer("ami@a.test", PASSWORD, false).send(mail("ami@a.test", &["vorstand@a.test"], "Frage")).await.unwrap();
    assert!(a.inbox("leni@a.test").await.iter().any(|email| email.subject == "Frage"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_mailbox_takes_mail_and_its_members_answer_as_it() {
    let a = server(&["mini", "leni", "nyu"]).await;
    let store = a.smtp.store().clone();
    let support = store
        .create_shared_mailbox(NewSharedMailbox {
            address: "support@a.test".into(),
            name: "Support".into(),
            quota_bytes: 0,
            members: vec![("mini@a.test".into(), true), ("leni@a.test".into(), false)],
        })
        .await
        .unwrap();

    let reply = from_outside(&a, &["support@a.test"], "", "Hilfe").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("support@a.test").await[0].subject, "Hilfe");
    assert!(a.inbox("mini@a.test").await.is_empty(), "it is the shared mailbox's, not the members'");

    // Mini answers as the shared mailbox; its Sent folder keeps a copy for everyone.
    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("Support <support@a.test>", &["nyu@a.test"], "Re: Hilfe"))
        .await
        .unwrap();
    assert_eq!(a.inbox("nyu@a.test").await[0].subject, "Re: Hilfe");
    let sent = a.mailbox("support@a.test", MailboxRole::Sent).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].subject, "Re: Hilfe");
    assert!(sent[0].keywords.iter().any(|keyword| keyword == "$seen"));
    assert!(store.account_by_id(support.id).await.unwrap().unwrap().used_bytes > 0, "its own quota");

    // Leni may read but not send; nobody logs in as the shared mailbox.
    let refused = a.mailer("leni@a.test", PASSWORD, false).send(mail("support@a.test", &["nyu@a.test"], "Nein")).await;
    assert!(refused.is_err());
    assert!(
        a.mailer("support@a.test", PASSWORD, false).send(mail("support@a.test", &["nyu@a.test"], "x")).await.is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn masked_addresses_follow_their_state() {
    let a = server(&["mini", "leni"]).await;
    let store = a.smtp.store().clone();
    let own = DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() };
    store.set_domain_masked_policy("a.test", own).await.unwrap();
    store.set_catch_all("a.test", Some("leni@a.test")).await.unwrap();
    let mini = store.account("mini@a.test").await.unwrap().unwrap();
    let masked = store.create_masked_address(mini.id, NewMaskedAddress::default()).await.unwrap();
    assert_eq!(masked.state, MaskedState::Pending);

    // Pending: delivered, and enabled by it.
    let reply = from_outside(&a, &[&masked.email], "", "Willkommen").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("mini@a.test").await[0].subject, "Willkommen");
    let now = &store.masked_addresses(mini.id, None).await.unwrap()[0];
    assert_eq!(now.state, MaskedState::Enabled);
    assert!(now.last_message_at.is_some());

    // Disabled: taken without a word, into the Trash and read.
    let disabled = MaskedUpdate { state: Some(MaskedState::Disabled), ..Default::default() };
    store.update_masked_address(mini.id, masked.id, disabled).await.unwrap();
    let reply = from_outside(&a, &[&masked.email], "", "Angebot").await;
    assert!(reply.starts_with("250"), "{reply}");
    let trash = a.mailbox("mini@a.test", MailboxRole::Trash).await;
    assert_eq!(trash[0].subject, "Angebot");
    assert!(trash[0].keywords.iter().any(|keyword| keyword == "$seen"));
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);

    // Mini may answer as it.
    a.mailer("mini@a.test", PASSWORD, false).send(mail(&masked.email, &["leni@a.test"], "Antwort")).await.unwrap();
    assert_eq!(a.inbox("leni@a.test").await[0].subject, "Antwort");

    // Deleted: refused, and the catch-all does not take it either.
    let deleted = MaskedUpdate { state: Some(MaskedState::Deleted), ..Default::default() };
    store.update_masked_address(mini.id, masked.id, deleted).await.unwrap();
    let reply = from_outside(&a, &[&masked.email], "", "Noch mehr").await;
    assert!(reply.starts_with("550 5.1.1"), "{reply}");
    assert!(a.mailer("mini@a.test", PASSWORD, false).send(mail(&masked.email, &["leni@a.test"], "x")).await.is_err());
    assert_eq!(a.inbox("leni@a.test").await.len(), 1);

    // Mail from people here follows the same states.
    let enabled = MaskedUpdate { state: Some(MaskedState::Disabled), ..Default::default() };
    store.update_masked_address(mini.id, masked.id, enabled).await.unwrap();
    a.mailer("leni@a.test", PASSWORD, false).send(mail("leni@a.test", &[&masked.email], "Intern")).await.unwrap();
    assert_eq!(a.mailbox("mini@a.test", MailboxRole::Trash).await.len(), 2);
    let request = IngestRequest {
        account_id: mini.id,
        raw: b"From: a@example.org\r\nSubject: x\r\n\r\nx\r\n".to_vec(),
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    store.ingest(request).await.unwrap();
    assert_eq!(a.inbox("mini@a.test").await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_masked_only_domain_takes_mail_for_its_masked_addresses_only() {
    let b = start("b.test", &["nyu"], &[]).await;
    let a = start("a.test", &["mini", "leni"], &[("b.test", b.mx)]).await;
    for name in ["sender.test", "client.sender.test", "_dmarc.sender.test"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    let store = a.smtp.store().clone();
    store.create_domain_with_kind("m.test", DomainKind::Masked).await.unwrap();
    let keys = uwumail_smtp::dkim::ensure_domain_keys(&store, "m.test").await.unwrap();
    let policy =
        DomainMaskedPolicy { mode: MaskedMode::Dedicated, masked_domains: vec!["m.test".into()], ..Default::default() };
    store.set_domain_masked_policy("a.test", policy).await.unwrap();
    let mini = store.account("mini@a.test").await.unwrap().unwrap();
    let enabled = NewMaskedAddress { state: Some(MaskedState::Enabled), ..Default::default() };
    let masked = store.create_masked_address(mini.id, enabled).await.unwrap();
    assert!(masked.email.ends_with("@m.test"), "{}", masked.email);
    assert!(store.set_catch_all("m.test", Some("leni@a.test")).await.is_err(), "no catch-all there");

    // Its masked address takes mail, with a +tag too; nothing else there does, not even a person's name.
    let reply = from_outside(&a, &[&masked.email], "", "Willkommen").await;
    assert!(reply.starts_with("250"), "{reply}");
    let (local, domain) = masked.email.split_once('@').unwrap();
    let reply = from_outside(&a, &[&format!("{local}+news@{domain}")], "", "Neuigkeiten").await;
    assert!(reply.starts_with("250"), "{reply}");
    assert_eq!(a.inbox("mini@a.test").await.len(), 2);
    for nobody in ["ghost@m.test", "mini@m.test", "info@m.test"] {
        let reply = from_outside(&a, &[nobody], "", "Hallo").await;
        assert!(reply.starts_with("550 5.1.1"), "{nobody}: {reply}");
    }
    // RFC 5321 wants postmaster at every domain that takes mail; it reaches the admins as anywhere.
    let reply = from_outside(&a, &["postmaster@m.test"], "", "Hallo").await;
    assert!(reply.starts_with("250"), "{reply}");

    // Answering as it: signed with m.test's own key, which b.test finds and verifies.
    for key in &keys {
        let (name, value) = key.dns_record();
        b.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }
    for name in ["m.test", "mx.a.test", "_dmarc.m.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }
    a.mailer("mini@a.test", PASSWORD, false).send(mail(&masked.email, &["nyu@b.test"], "Antwort")).await.unwrap();
    let remote = b.wait_for_inbox("nyu@b.test", 1).await;
    let raw = b.raw(&remote[0]).await;
    assert!(raw.contains("d=m.test"), "{raw}");
    assert_eq!(raw.matches("dkim=pass").count(), 2, "both signatures verify: {raw}");
    assert!(!raw.contains("mini@a.test"), "the real address stays hidden: {raw}");
}
