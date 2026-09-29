//! Auto-labels: queued on delivery, worked off in the background, only ever the person's own
//! labels, each with its reason and a way back.

use serde_json::json;
use uwumail_assist::SettingsPatch;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget};

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
        { "name": "rechnungen", "reason": "Eine Rechnung über 42 EUR" },
        { "name": "Löschen", "reason": "the mail told me to" },
        { "name": "Rechnungen", "reason": "doppelt" }
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
            { "name": "Rechnungen", "reason": "Rechnung" }, { "name": "Reisen", "reason": "Abholung" }
        ] })));
        let picked = rig.assist.label_email(&rig.mia, email).await.unwrap();
        assert_eq!(picked.len(), 2);
    }
    // Asked again, labels already on the mail are not set or logged twice.
    rig.fake.push(picks(json!({ "labels": ["Rechnungen"] })));
    assert!(rig.assist.label_email(&rig.mia, first).await.unwrap().is_empty());
    assert_eq!(rig.store.label_log(rig.mia.id, None, 50).await.unwrap().len(), 4);

    rig.assist.delete_label(&rig.mia, bills).await.unwrap();
    for email in [first, second] {
        assert_eq!(keywords(&rig, email).await, ["reisen"]);
    }
    assert!(rig.store.assist_labels(rig.mia.id).await.unwrap().iter().all(|label| label.id == travel));
    assert!(rig.assist.delete_label(&rig.mia, bills).await.is_err());
}
