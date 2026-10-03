//! Auto-labels: queued on delivery, worked off in the background, only ever the person's own
//! labels, each with its reason and a way back.

use serde_json::json;
use uwumail_assist::SettingsPatch;
use uwumail_store::{EmailUpdate, IngestRequest, KeywordsChange, MailboxRole, MailboxTarget, MailboxesChange};

use crate::common::{INVOICE, Reply, Rig, chat, rig};

async fn labelled_rig() -> (Rig, i64, i64) {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    let bills = rig
        .store
        .create_assist_label(rig.mia.id, "Rechnungen".into(), "Rechnungen und Zahlungserinnerungen".into(), None)
        .await
        .unwrap();
    let travel =
        rig.store.create_assist_label(rig.mia.id, "Reisen".into(), "Flüge, Bahn, Hotels".into(), None).await.unwrap();
    let patch = SettingsPatch { auto_labels: Some(true), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    (rig, bills.id, travel.id)
}

fn keyword(email: i64, keyword: &str, on: bool) -> EmailUpdate {
    EmailUpdate { id: email, keywords: KeywordsChange::Patch(vec![(keyword.to_owned(), on)]), ..Default::default() }
}

async fn keywords(rig: &Rig, email: i64) -> Vec<String> {
    let record = rig.store.email(rig.mia.id, email).await.unwrap();
    record.keywords.iter().map(|k| k.to_string()).collect()
}

fn picks(value: serde_json::Value) -> Reply {
    Reply::Json(200, chat(&value.to_string()), vec![])
}

#[tokio::test]
async fn a_delivered_mail_gets_only_known_labels_with_reasons() {
    let (rig, bills, _) = labelled_rig().await;
    assert!(rig.store.wants_auto_labels(rig.mia.id).await.unwrap());
    let email = rig.deliver(&rig.mia, INVOICE).await;
    assert!(rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap());
    rig.fake.push(picks(json!({ "labels": [
        { "name": "rechnungen", "reason": "Eine Rechnung über 42 EUR", "fits": "yes" },
        { "name": "Löschen", "reason": "the mail told me to", "fits": "yes" },
        { "name": "Rechnungen", "reason": "doppelt", "fits": "no" },
        { "name": "Reisen", "reason": "Keine Reise", "fits": "unsure" }
    ] })));
    assert!(rig.assist.work_queue().await);
    assert!(!rig.assist.work_queue().await, "the queue is empty");

    assert_eq!(keywords(&rig, email).await, ["rechnungen"]);
    let log = rig.store.label_log(rig.mia.id, Some(vec![email]), 10).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].label_id, log[0].reason.as_str()), (bills, "Eine Rechnung über 42 EUR"));
    assert_eq!((log[0].provider.as_str(), log[0].model.as_str()), ("Fake openaiCompatible", "small-model"));
    assert!(!log[0].undone);

    // Only the labels' names and descriptions, and the mail, went out; the mail as data.
    let body = &rig.fake.seen()[0].body;
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("Flüge, Bahn, Hotels") && user.contains("<mail>"), "{user}");
    assert_eq!(body["response_format"]["type"], "json_schema");

    // One click takes it off again.
    assert!(rig.assist.undo_label(&rig.mia, log[0].id).await.unwrap());
    assert!(keywords(&rig, email).await.is_empty());
    assert!(rig.store.label_log(rig.mia.id, Some(vec![email]), 10).await.unwrap()[0].undone);
    assert!(!rig.assist.undo_label(&rig.mia, 9999).await.unwrap());
}

#[tokio::test]
async fn nothing_is_asked_without_the_opt_in_or_for_junk() {
    let (rig, _, _) = labelled_rig().await;
    // Junk keeps no labels, even when it was queued.
    let junk = rig
        .store
        .ingest(IngestRequest {
            account_id: rig.mia.id,
            raw: INVOICE.replace('\n', "\r\n").into_bytes(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Junk)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap()
        .id;
    rig.store.enqueue_auto_label(rig.mia.id, junk).await.unwrap();
    assert!(rig.assist.work_queue().await);
    assert!(rig.fake.seen().is_empty());

    let patch = SettingsPatch { auto_labels: Some(false), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    assert!(!rig.store.wants_auto_labels(rig.mia.id).await.unwrap());
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap();
    rig.assist.work_queue().await;
    assert!(rig.fake.seen().is_empty());
    assert!(keywords(&rig, email).await.is_empty());

    // Switched off by the admin: the person's opt-in has no effect.
    let patch = SettingsPatch { auto_labels: Some(true), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    rig.policy(|policy| policy.features.auto_labels = false).await;
    rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap();
    rig.assist.work_queue().await;
    assert!(rig.fake.seen().is_empty());
}

#[tokio::test]
async fn a_busy_provider_is_tried_again_later_and_a_broken_one_not() {
    let (rig, _, _) = labelled_rig().await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap();
    rig.fake.push(Reply::Json(429, json!({ "error": { "message": "slow down" } }), vec![]));
    assert!(rig.assist.work_queue().await);
    // Still queued, but not due now.
    assert!(rig.store.due_label_jobs(10).await.unwrap().is_empty());
    let next = rig.store.next_label_job_at().await.unwrap().expect("still queued");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert!(next > now + 30, "waits a while");

    // A refused key is not going to get better.
    let other = rig.deliver(&rig.mia, &INVOICE.replace("4711@shop", "4712@shop")).await;
    rig.store.enqueue_auto_label(rig.mia.id, other).await.unwrap();
    rig.fake.push(Reply::Json(401, json!({ "error": { "message": "bad key" } }), vec![]));
    assert!(rig.assist.work_queue().await);
    let db = rusqlite::Connection::open(rig.dir.path().join("uwumail.db")).unwrap();
    let queued: Vec<(i64, i64)> = db
        .prepare("SELECT email_id, attempts FROM assist_label_queue ORDER BY email_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(queued, [(email, 1)]);
}

#[tokio::test]
async fn deleting_a_label_takes_it_off_every_mail() {
    let (rig, bills, travel) = labelled_rig().await;
    let first = rig.deliver(&rig.mia, INVOICE).await;
    let second = rig.deliver(&rig.mia, &INVOICE.replace("4711@shop", "4712@shop")).await;
    for email in [first, second] {
        rig.fake.push(picks(json!({ "labels": [
            { "name": "Rechnungen", "reason": "Rechnung", "fits": "yes" },
            { "name": "Reisen", "reason": "Abholung", "fits": "unsure" }
        ] })));
        let picked = rig.assist.label_email(&rig.mia, email).await.unwrap();
        assert_eq!(picked.iter().map(|p| p.label.id).collect::<Vec<_>>(), [bills]);
        // Travel goes on by hand.
        rig.store.update_emails(rig.mia.id, vec![keyword(email, "reisen", true)]).await.unwrap();
    }
    // Two labels on the mail: nothing is asked, set or logged twice.
    let asked = rig.fake.seen().len();
    assert!(rig.assist.label_email(&rig.mia, first).await.unwrap().is_empty());
    assert_eq!(rig.fake.seen().len(), asked);
    assert_eq!(rig.store.label_log(rig.mia.id, None, 50).await.unwrap().len(), 2);

    rig.assist.delete_label(&rig.mia, bills).await.unwrap();
    for email in [first, second] {
        assert_eq!(keywords(&rig, email).await, ["reisen"]);
    }
    let left = rig.store.assist_labels(rig.mia.id).await.unwrap();
    assert!(left.iter().any(|label| label.id == travel) && !left.iter().any(|label| label.id == bills));
    assert!(rig.assist.delete_label(&rig.mia, bills).await.is_err());
}

/// AI-04 of the 0.18.0 audit: one person's queue is bounded, and a batch takes one job per person,
/// so a full queue or a slow provider of one person holds up nobody else.
#[tokio::test]
async fn one_persons_queue_holds_up_nobody_else() {
    let (rig, _, _) = labelled_rig().await;
    let leni = crate::common::account(&rig.store, "leni@example.org").await;
    rig.store.create_assist_label(leni.id, "Arbeit".into(), "Alles von der Arbeit".into(), None).await.unwrap();
    let patch = SettingsPatch { auto_labels: Some(true), ..SettingsPatch::default() };
    rig.assist.set_settings(&leni, patch).await.unwrap();
    let mut queued = 0;
    for email in 1..=250 {
        if rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap() {
            queued += 1;
        }
    }
    assert_eq!(queued, 200, "at most 200 waiting per person");
    assert!(rig.store.enqueue_auto_label(leni.id, 1).await.unwrap(), "someone else still gets in");
    let due = rig.store.due_label_jobs(20).await.unwrap();
    let mut accounts: Vec<i64> = due.iter().map(|job| job.account_id).collect();
    accounts.sort();
    assert_eq!(accounts, [rig.mia.id, leni.id]);
}

const NEWSLETTER: &str = "From: Shop News <noreply@news.shop.example>
To: Mia <mia@example.org>
Subject: Hallo Mia
Date: Mon, 28 Sep 2026 10:00:00 +0000
Message-ID: <news-1@shop.example>
List-Unsubscribe: <https://news.shop.example/unsubscribe>
Content-Type: text/plain; charset=utf-8

Liebe Mia, schön dass du da bist! Bis bald.
";

#[tokio::test]
async fn the_facts_overrule_the_model() {
    let (rig, _, _) = labelled_rig().await;
    let email = rig.deliver(&rig.mia, NEWSLETTER).await;
    // A mass mail from a no-reply address is not personal, however warmly it greets.
    rig.fake.push(picks(json!({ "labels": [
        { "name": "Personal", "reason": "Greets Mia by name", "fits": "yes" }
    ] })));
    assert!(rig.assist.label_email(&rig.mia, email).await.unwrap().is_empty());
    let user = rig.fake.seen()[0].body["messages"][1]["content"].as_str().unwrap().to_owned();
    assert!(user.contains("List-Unsubscribe header: yes") && user.contains("sender type: noReply"), "{user}");
    // The base labels go out with their definitions and examples.
    assert!(user.contains("Personal: ") && user.contains("does not belong:"), "{user}");
}

#[tokio::test]
async fn a_model_that_says_yes_to_everything_is_not_believed() {
    let (rig, _, _) = labelled_rig().await;
    let email = rig.deliver(&rig.mia, NEWSLETTER).await;
    rig.fake.push(picks(json!({ "labels": [
        { "name": "Newsletter", "reason": "a", "fits": "yes" },
        { "name": "Reisen", "reason": "b", "fits": "yes" },
        { "name": "Rechnungen", "reason": "c", "fits": "yes" }
    ] })));
    assert!(rig.assist.label_email(&rig.mia, email).await.unwrap().is_empty());
    // Two that exclude each other are not believed either.
    rig.fake.push(picks(json!({ "labels": [
        { "name": "Newsletter", "reason": "a", "fits": "yes" },
        { "name": "Promotions", "reason": "b", "fits": "yes" }
    ] })));
    assert!(rig.assist.label_email(&rig.mia, email).await.unwrap().is_empty());
    // One yes is.
    rig.fake.push(picks(json!({ "labels": [{ "name": "Newsletter", "reason": "News of a shop", "fits": "yes" }] })));
    let picked = rig.assist.label_email(&rig.mia, email).await.unwrap();
    assert_eq!(picked.iter().map(|p| (p.label.name.as_str(), p.source)).collect::<Vec<_>>(), [("Newsletter", "ai")]);
}

fn trip(n: usize) -> String {
    format!(
        "From: Bahn <tickets@rail.example>\nTo: Mia <mia@example.org>\nSubject: Deine Reise nach Ort {n}\n\
Date: Mon, 28 Sep 2026 10:00:00 +0000\nMessage-ID: <trip-{n}@rail.example>\nContent-Type: text/plain; charset=utf-8\n\n\
Gute Fahrt nach Ort {n}! Abfahrt Gleis {n}.\n"
    )
}

#[tokio::test]
async fn similar_mails_decide_by_embeddings_without_asking_the_model() {
    let (rig, _, travel) = labelled_rig().await;
    let embedder =
        rig.server_provider("embeddingsCompatible", json!({ "model": "embed-model", "fastModel": null })).await;
    for n in 1..=3 {
        let email = rig.deliver(&rig.mia, &trip(n)).await;
        rig.store.update_emails(rig.mia.id, vec![keyword(email, "reisen", true)]).await.unwrap();
    }
    assert!(rig.assist.learn_labels().await);
    let email = rig.deliver(&rig.mia, &trip(4)).await;
    let vector = |x: f32| json!({ "object": "embedding", "embedding": [x, 0.2, 0.1] });
    rig.fake.push(Reply::Json(
        200,
        json!({ "data": [vector(1.0), vector(0.9), vector(1.1), vector(1.0)], "usage": { "prompt_tokens": 80 } }),
        vec![],
    ));
    let picked = rig.assist.label_email(&rig.mia, email).await.unwrap();
    assert_eq!(picked.iter().map(|p| (p.label.id, p.source)).collect::<Vec<_>>(), [(travel, "similar")]);
    let seen = rig.fake.seen();
    assert_eq!(seen.len(), 1, "the model was not asked");
    assert!(seen[0].path.ends_with("/embeddings"), "{}", seen[0].path);
    assert_eq!(seen[0].body["model"], "embed-model");
    assert_eq!(seen[0].body["input"].as_array().unwrap().len(), 4, "the new mail and three labeled ones");
    // The labeled mails keep their vectors: the next mail only sends itself.
    let vectors = rig.store.label_vectors(rig.mia.id, "embed-model".into()).await.unwrap();
    assert_eq!(vectors.len(), 3);
    assert!(vectors.iter().all(|v| v.vector.len() == 4 + 3));
    let today = rig.assist.today(&rig.mia).await.unwrap();
    assert!(today.iter().all(|usage| usage.provider_id != embedder), "embeddings are no provider to choose");
}

/// Security review 0.22 LABELS22-M1: mail only goes to the embeddings provider while AI labels
/// may use a server model for the person, and mail in Junk or Trash never does.
#[tokio::test]
async fn embeddings_follow_the_switches_and_the_persons_choice() {
    let embedded = |rig: &Rig| rig.fake.seen().iter().filter(|seen| seen.path.ends_with("/embeddings")).count();
    let (rig, _, _) = labelled_rig().await;
    rig.server_provider("embeddingsCompatible", json!({ "model": "embed-model", "fastModel": null })).await;
    let mut labeled = Vec::new();
    for n in 1..=3 {
        let email = rig.deliver(&rig.mia, &trip(n)).await;
        rig.store.update_emails(rig.mia.id, vec![keyword(email, "reisen", true)]).await.unwrap();
        labeled.push(email);
    }
    assert!(rig.assist.learn_labels().await);

    // The admin switched AI labels off: nothing is embedded, nothing asked.
    rig.policy(|policy| policy.features.set("autoLabels", false)).await;
    let email = rig.deliver(&rig.mia, &trip(4)).await;
    let _ = rig.assist.label_email(&rig.mia, email).await;
    assert_eq!(embedded(&rig), 0, "switched off on the server");
    rig.policy(|policy| policy.features.set("autoLabels", true)).await;

    // The person switched AI labels off.
    let off = SettingsPatch { auto_labels: Some(false), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, off).await.unwrap();
    let _ = rig.assist.label_email(&rig.mia, email).await;
    assert_eq!(embedded(&rig), 0, "switched off by the person");
    let on = SettingsPatch { auto_labels: Some(true), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, on).await.unwrap();

    // Mail in Junk or Trash is no neighbour and is not backfilled.
    let roles = rig.store.mailboxes(rig.mia.id).await.unwrap();
    let junk = roles.iter().find(|m| m.role == Some(MailboxRole::Junk)).unwrap().id;
    let moved = EmailUpdate { id: labeled[0], mailboxes: MailboxesChange::Replace(vec![junk]), ..Default::default() };
    rig.store.update_emails(rig.mia.id, vec![moved]).await.unwrap();
    let vector = |x: f32| json!({ "object": "embedding", "embedding": [x, 0.2, 0.1] });
    rig.fake.push(Reply::Json(
        200,
        json!({ "data": [vector(1.0), vector(0.9), vector(1.1)], "usage": { "prompt_tokens": 60 } }),
        vec![],
    ));
    let _ = rig.assist.label_email(&rig.mia, email).await;
    let seen = rig.fake.seen();
    let request = seen.iter().find(|seen| seen.path.ends_with("/embeddings")).expect("embedded with a server model");
    assert_eq!(request.body["input"].as_array().unwrap().len(), 3, "the new mail and the two labeled outside Junk");
    let vectors = rig.store.label_vectors(rig.mia.id, "embed-model".into()).await.unwrap();
    assert!(vectors.iter().all(|v| v.email_id != labeled[0]), "not a neighbour from Junk");
    assert!(rig.store.label_token_sets(rig.mia.id).await.unwrap().iter().all(|t| t.email_id != labeled[0]));
}

/// Security review 0.22 LABELS22-M1: someone who picked a personal model for labels does not
/// have their mail sent to the server's embeddings provider.
#[tokio::test]
async fn a_personal_model_for_labels_keeps_mail_from_the_embeddings() {
    let (rig, _, _) = labelled_rig().await;
    rig.server_provider("embeddingsCompatible", json!({ "model": "embed-model", "fastModel": null })).await;
    rig.policy(|policy| policy.allow_personal = true).await;
    let own: uwumail_assist::ProviderInput = serde_json::from_value(json!({
        "name": "Own", "kind": "openaiCompatible", "baseUrl": "https://llm.example.net/v1",
        "apiKey": "sk-test-abcdefgh1234", "model": "big-model", "fastModel": "small-model"
    }))
    .unwrap();
    let personal = rig.assist.create_personal_provider(&rig.mia, own).await.unwrap();
    let choice = serde_json::from_value(json!({ "providerId": personal.id })).unwrap();
    let patch = SettingsPatch {
        features: Some([("autoLabels".to_owned(), Some(choice))].into_iter().collect()),
        ..SettingsPatch::default()
    };
    rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    for n in 1..=2 {
        let email = rig.deliver(&rig.mia, &trip(n)).await;
        rig.store.update_emails(rig.mia.id, vec![keyword(email, "reisen", true)]).await.unwrap();
    }
    assert!(rig.assist.learn_labels().await);
    let email = rig.deliver(&rig.mia, &trip(3)).await;
    let _ = rig.assist.label_email(&rig.mia, email).await;
    assert!(rig.fake.seen().iter().all(|seen| !seen.path.ends_with("/embeddings")), "{:?}", rig.fake.seen().len());
}

/// Security review 0.22 LABELS22-L1: a contact's address in From makes a known sender only when
/// this server's authentication backs it.
#[tokio::test]
async fn a_contact_is_only_known_when_authentication_backs_the_address() {
    use uwumail_store::{DavKind, DavPrecondition, DavWrite, NewDavCollection};
    let (rig, _, _) = labelled_rig().await;
    let book = NewDavCollection { slug: "contacts".into(), display_name: "Kontakte".into(), ..Default::default() };
    let book = rig.store.dav_collections(rig.mia.id, DavKind::Addressbook, book).await.unwrap()[0].clone();
    let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:anna\r\nFN:Anna\r\nEMAIL:anna@example.com\r\nEND:VCARD\r\n";
    let write = DavWrite {
        name: "anna.vcf".into(),
        content: card.into(),
        uid: "anna".into(),
        component: "VCARD".into(),
        starts_at: None,
        ends_at: None,
    };
    rig.store.dav_put(rig.mia.id, book.id, write, DavPrecondition::default()).await.unwrap();
    let mail = |headers: &str, n: u32| {
        format!(
            "{headers}From: Anna <anna@example.com>\nTo: Mia <mia@example.org>\nSubject: Hi {n}\n\
Date: Mon, 28 Sep 2026 10:00:00 +0000\nMessage-ID: <anna-{n}@example.com>\n\nHey Mia, wie geht's?\n"
        )
    };
    let known_line = |rig: &Rig, at: usize| {
        let user = rig.fake.seen()[at].body["messages"][1]["content"].as_str().unwrap().to_owned();
        user.lines().find(|line| line.contains("sender known to the reader")).map(str::to_owned).unwrap_or_default()
    };

    let forged = rig.deliver(&rig.mia, &mail("", 1)).await;
    rig.fake.push(picks(json!({ "labels": [] })));
    let _ = rig.assist.label_email(&rig.mia, forged).await;
    assert!(known_line(&rig, 0).ends_with("no"), "{}", known_line(&rig, 0));

    let ours = "Received: from mail.example.com\n\tby mx.example.org (UwUMail) with ESMTPS id 1\n\
Authentication-Results: mx.example.org; spf=pass smtp.mailfrom=example.com; dkim=pass header.d=example.com; dmarc=pass header.from=example.com\n";
    let real = rig.deliver(&rig.mia, &mail(ours, 2)).await;
    rig.fake.push(picks(json!({ "labels": [] })));
    let _ = rig.assist.label_email(&rig.mia, real).await;
    assert!(known_line(&rig, 1).ends_with("yes"), "{}", known_line(&rig, 1));
}

#[tokio::test]
async fn a_label_taken_off_a_senders_mail_is_not_asked_about_again() {
    let (rig, _, _) = labelled_rig().await;
    rig.store.ensure_base_labels(rig.mia.id, "en").await.unwrap();
    let first = rig.deliver(&rig.mia, NEWSLETTER).await;
    rig.store.update_emails(rig.mia.id, vec![keyword(first, "newsletter", true)]).await.unwrap();
    rig.store.update_emails(rig.mia.id, vec![keyword(first, "newsletter", false)]).await.unwrap();
    let second = rig.deliver(&rig.mia, &NEWSLETTER.replace("news-1@", "news-2@")).await;
    rig.fake.push(picks(json!({ "labels": [{ "name": "Newsletter", "reason": "News", "fits": "yes" }] })));
    assert!(rig.assist.label_email(&rig.mia, second).await.unwrap().is_empty());
    let user = rig.fake.seen()[0].body["messages"][1]["content"].as_str().unwrap().to_owned();
    assert!(!user.contains("- Newsletter:"), "{user}");
}
