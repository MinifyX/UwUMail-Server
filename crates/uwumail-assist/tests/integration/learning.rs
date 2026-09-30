//! Labels learn from the person: what they put on or take off by hand teaches senders and the
//! classifier, what the server does by itself teaches nothing, and the model only judges the labels
//! not on a mail yet.

use serde_json::json;
use uwumail_assist::{SettingsPatch, SuggestArgs};
use uwumail_store::{EmailUpdate, KeywordsChange, MailboxRole, MailboxesChange};

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
    (rig, bills.id, travel.id)
}

fn keyword(email: i64, keyword: &str, on: bool) -> EmailUpdate {
    EmailUpdate { id: email, keywords: KeywordsChange::Patch(vec![(keyword.to_owned(), on)]), ..Default::default() }
}

async fn keywords(rig: &Rig, email: i64) -> Vec<String> {
    let record = rig.store.email(rig.mia.id, email).await.unwrap();
    let mut keywords: Vec<String> = record.keywords.iter().map(|k| k.to_string()).collect();
    keywords.sort();
    keywords
}

fn count(rig: &Rig, sql: &str) -> i64 {
    let db = rusqlite::Connection::open(rig.dir.path().join("uwumail.db")).unwrap();
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

async fn knowledge(rig: &Rig) -> uwumail_labels::Knowledge {
    rig.store.label_knowledge(rig.mia.id, "billing@shop.example".into(), vec![], vec![]).await.unwrap()
}

fn mail(n: usize, from: &str) -> String {
    INVOICE.replace("4711@shop", &format!("{n}@shop")).replace("billing@shop.example", from)
}

#[tokio::test]
async fn labels_learn_from_the_hand_and_never_from_the_server() {
    let (rig, bills, _) = labelled_rig().await;
    let ordinary = rig.deliver(&rig.mia, &mail(1, "news@other.example")).await;
    let first = rig.deliver(&rig.mia, &mail(2, "billing@shop.example")).await;
    let second = rig.deliver(&rig.mia, &mail(3, "Billing@Shop.example")).await;

    rig.store.update_emails(rig.mia.id, vec![keyword(first, "rechnungen", true)]).await.unwrap();
    assert_eq!(rig.store.label_training().await.unwrap().len(), 1);
    assert!(rig.assist.learn_labels().await);
    assert!(!rig.assist.learn_labels().await, "learned once");
    let counts = rig.store.label_counts(rig.mia.id).await.unwrap();
    assert_eq!((counts[&bills].total, counts[&bills].unread, counts[&bills].examples), (1, 1, 1));
    // The labeled mail and one ordinary inbox mail beside it.
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM label_examples"), 2);
    assert_eq!(
        count(&rig, &format!("SELECT COUNT(*) FROM label_examples WHERE email_id IN ({ordinary}, {second})")),
        1
    );

    // One hand-labeling does not make a learned sender yet, two do.
    assert_eq!(knowledge(&rig).await.senders.get(&bills), Some(&1));
    rig.store.update_emails(rig.mia.id, vec![keyword(second, "rechnungen", true)]).await.unwrap();
    assert_eq!(knowledge(&rig).await.senders.get(&bills), Some(&2));

    // What the server puts on or takes off teaches nothing.
    rig.assist.learn_labels().await;
    let third = rig.deliver(&rig.mia, &mail(4, "billing@shop.example")).await;
    rig.store.update_emails_by_server(rig.mia.id, vec![keyword(third, "rechnungen", true)]).await.unwrap();
    rig.store.update_emails_by_server(rig.mia.id, vec![keyword(first, "rechnungen", false)]).await.unwrap();
    assert!(rig.store.label_training().await.unwrap().is_empty());
    assert_eq!(knowledge(&rig).await.senders.get(&bills), Some(&2));

    // Taken off by hand, the sender is forgotten and the mail becomes an example without it.
    rig.store.update_emails(rig.mia.id, vec![keyword(second, "rechnungen", false)]).await.unwrap();
    assert_eq!(knowledge(&rig).await.senders.get(&bills), None);
    assert!(rig.assist.learn_labels().await);
    assert_eq!(rig.store.label_counts(rig.mia.id).await.unwrap()[&bills].examples, 1);

    // Counts leave out mail that is only in the Trash.
    let trash = rig.store.mailboxes(rig.mia.id).await.unwrap();
    let trash = trash.iter().find(|m| m.role == Some(MailboxRole::Trash)).unwrap().id;
    let moved = EmailUpdate { id: third, mailboxes: MailboxesChange::Replace(vec![trash]), ..Default::default() };
    rig.store.update_emails(rig.mia.id, vec![moved]).await.unwrap();
    assert_eq!(rig.store.label_counts(rig.mia.id).await.unwrap()[&bills].total, 0);
}

#[tokio::test]
async fn the_model_judges_only_labels_not_set_and_undo_counts_as_the_hand() {
    let (rig, _, travel) = labelled_rig().await;
    rig.assist.set_settings(&rig.mia, SettingsPatch { auto_labels: Some(true), ..Default::default() }).await.unwrap();
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.store.update_emails_by_server(rig.mia.id, vec![keyword(email, "rechnungen", true)]).await.unwrap();

    rig.fake.push(Reply::Json(
        200,
        chat(&json!({ "labels": [{ "name": "Reisen", "reason": "Eine Abholung.", "fits": true }] }).to_string()),
        vec![],
    ));
    let picks = rig.assist.label_email(&rig.mia, email).await.unwrap();
    assert_eq!(picks.iter().map(|p| p.label.id).collect::<Vec<_>>(), [travel]);
    let body = &rig.fake.seen()[0].body;
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("Reisen") && !user.contains("Zahlungserinnerungen"), "{user}");
    assert_eq!(keywords(&rig, email).await, ["rechnungen", "reisen"]);
    assert!(rig.store.label_training().await.unwrap().is_empty(), "the model teaches nothing");
    let log = rig.store.label_log(rig.mia.id, Some(vec![email]), 10).await.unwrap();
    assert_eq!((log[0].source.as_str(), log[0].code.as_str()), ("ai", "ai"));

    // Every label on the mail: nothing to ask.
    assert!(rig.assist.label_email(&rig.mia, email).await.unwrap().is_empty());
    assert_eq!(rig.fake.seen().len(), 1);

    // Undo is the person taking the label off.
    assert!(rig.assist.undo_label(&rig.mia, log[0].id).await.unwrap());
    let training = rig.store.label_training().await.unwrap();
    assert_eq!(training.iter().map(|t| (t.label_id, t.positive)).collect::<Vec<_>>(), [(travel, false)]);
}

#[tokio::test]
async fn suggest_judges_every_label_and_proposes_new_ones_only_when_none_fits() {
    let (rig, bills, travel) = labelled_rig().await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.store.update_emails(rig.mia.id, vec![keyword(email, "reisen", true)]).await.unwrap();

    let answer = json!({
        "verdicts": [
            { "name": "Reisen", "reason": "Keine Reise.", "fits": false },
            { "name": "Rechnungen", "reason": "Eher ein Angebot.", "fits": false }
        ],
        "newLabels": [
            { "name": "Shop", "description": "Mails von Shops", "color": "#123ABC", "reason": "Vom Shop." },
            { "name": "rechnungen", "description": "", "color": "#000000", "reason": "gibt es schon" }
        ]
    });
    rig.fake.push(Reply::Json(200, chat(&answer.to_string()), vec![]));
    let args = SuggestArgs { email_id: email, language: Some("de".into()), ..Default::default() };
    let result = rig.assist.suggest_labels(&rig.mia, args).await.unwrap();
    let verdicts: Vec<(Option<i64>, bool, bool)> =
        result.verdicts.iter().map(|v| (v.label_id, v.fits, v.is_set)).collect();
    assert_eq!(verdicts, [(Some(bills), false, false), (Some(travel), false, true)]);
    assert_eq!(result.new_labels.len(), 1);
    assert_eq!((result.new_labels[0].name.as_str(), result.new_labels[0].color.as_str()), ("Shop", "#123abc"));
    let body = &rig.fake.seen()[0].body;
    let schema = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(schema["required"], json!(["verdicts", "newLabels"]));
    assert_eq!(schema["properties"]["verdicts"]["items"]["required"], json!(["name", "reason", "fits"]));
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("in German"));
    assert_eq!(keywords(&rig, email).await, ["reisen"], "suggest changes nothing");
    let (rows, _) = rig.assist.usage(&rig.mia, 1).await.unwrap();
    assert_eq!((rows[0].feature.as_str(), rows[0].requests), ("autoLabels", 1));

    // Without suggestNew the model is not asked for labels, and one that fits keeps it from them.
    let answer = json!({
        "verdicts": [{ "name": "Rechnungen", "reason": "Eine Rechnung.", "fits": true }],
        "newLabels": [{ "name": "Shop", "description": "", "color": "#123abc", "reason": "x" }]
    });
    rig.fake.push(Reply::Json(200, chat(&answer.to_string()), vec![]));
    let args = SuggestArgs { email_id: email, suggest_new: false, ..Default::default() };
    let result = rig.assist.suggest_labels(&rig.mia, args).await.unwrap();
    assert!(result.new_labels.is_empty());
    let schema = &rig.fake.seen()[1].body["response_format"]["json_schema"]["schema"];
    assert_eq!(schema["required"], json!(["verdicts"]));
}

fn club(n: usize) -> String {
    format!(
        "From: Verein <vorstand{n}@sportverein.example>\nTo: Mia <mia@example.org>\nSubject: Training und Vereinsheim \
Woche {n}\nMessage-ID: <club{n}@sportverein.example>\n\nLiebe Mitglieder, das Training der Jugendmannschaft findet \
im Vereinsheim statt. Beiträge bitte an den Kassenwart.\n"
    )
}

fn other(n: usize) -> String {
    let topics = [
        "Die Sitzung zum Projektplan verschiebt sich, bitte Unterlagen prüfen.",
        "Your cloud storage is almost full, upgrade your plan today.",
        "Hallo Mia, wie war der Urlaub? Lass uns bald telefonieren.",
        "Neue Angebote im Onlineshop: Schuhe und Jacken reduziert.",
    ];
    format!(
        "From: someone{n}@example.com\nTo: Mia <mia@example.org>\nSubject: Nachricht {n}\nMessage-ID: \
<other{n}@example.com>\n\n{}\n",
        topics[n % topics.len()]
    )
}

/// What the classifier of `label` says of a mail, from what the store learned.
async fn classify(rig: &Rig, label: i64, raw: &str) -> Option<uwumail_labels::Verdict> {
    let mail = uwumail_labels::Mail::parse(raw.replace('\n', "\r\n").as_bytes());
    let tokens: Vec<i64> =
        uwumail_labels::tokens(&mail).iter().map(|token| uwumail_labels::token_hash(token)).collect();
    let knowledge = rig.store.label_knowledge(rig.mia.id, String::new(), tokens.clone(), vec![label]).await.unwrap();
    knowledge.models.get(&label).and_then(|model| model.classify(&tokens))
}

#[tokio::test]
async fn the_classifier_acts_once_it_learned_enough_from_the_hand() {
    let rig = rig().await;
    let label = rig.store.create_assist_label(rig.mia.id, "Verein".into(), String::new(), None).await.unwrap();
    for n in 0..20 {
        rig.deliver(&rig.mia, &other(n)).await;
    }
    for n in 0..14 {
        let email = rig.deliver(&rig.mia, &club(n)).await;
        rig.store.update_emails(rig.mia.id, vec![keyword(email, "verein", true)]).await.unwrap();
    }
    assert!(rig.assist.learn_labels().await);
    assert!(classify(&rig, label.id, &club(99)).await.is_none(), "14 examples are not enough");
    let email = rig.deliver(&rig.mia, &club(14)).await;
    rig.store.update_emails(rig.mia.id, vec![keyword(email, "verein", true)]).await.unwrap();
    assert!(rig.assist.learn_labels().await);
    let counts = rig.store.label_counts(rig.mia.id).await.unwrap();
    assert_eq!(counts[&label.id].examples, 15);
    let verdict = classify(&rig, label.id, &club(99)).await.expect("sure about a mail like the club's");
    assert!(verdict.probability >= 0.99 && verdict.examples == 15, "{verdict:?}");
    assert!(classify(&rig, label.id, &other(99)).await.is_none());
}

/// Security audit 0.21.0 LABELS-M1 and LABELS-L1: putting a label on and off again queues one
/// hand-labeling per email and label, the last change winning; a label learns only what its
/// switches say, and nothing while labels without a model are off; one account's full queue stops
/// at its limit and holds up nobody else; labeling in a folder shared with someone teaches the
/// owner's labels nothing.
#[tokio::test]
async fn learning_is_bounded_and_follows_the_switches() {
    let (rig, bills, travel) = labelled_rig().await;
    let first = rig.deliver(&rig.mia, &mail(1, "billing@shop.example")).await;
    for on in [true, false, true, false, true] {
        rig.store.update_emails(rig.mia.id, vec![keyword(first, "rechnungen", on)]).await.unwrap();
    }
    let jobs = rig.store.label_training().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].positive && jobs[0].label_id == bills, "the last change wins");

    let quiet = uwumail_store::AssistLabelWrite {
        learn_senders: false,
        classifier: false,
        ..uwumail_store::AssistLabelWrite::simple("Reisen".into(), "Flüge, Bahn, Hotels".into(), None)
    };
    rig.store.update_assist_label_with(rig.mia.id, travel, quiet).await.unwrap();
    rig.store.update_emails(rig.mia.id, vec![keyword(first, "reisen", true)]).await.unwrap();
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM label_training"), 1, "no classifier, nothing queued");
    assert_eq!(count(&rig, &format!("SELECT COUNT(*) FROM label_senders WHERE label_id = {travel}")), 0);

    rig.store.set_non_ai_labels(rig.mia.id, false).await.unwrap();
    let second = rig.deliver(&rig.mia, &mail(2, "billing@shop.example")).await;
    rig.store.update_emails(rig.mia.id, vec![keyword(second, "rechnungen", true)]).await.unwrap();
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM label_training"), 1, "switched off, nothing learned");
    assert_eq!(knowledge(&rig).await.senders.get(&bills), Some(&1));
    rig.store.set_non_ai_labels(rig.mia.id, true).await.unwrap();

    // In a share, the owner's labels learn nothing from whoever it is shared with.
    let third = rig.deliver(&rig.mia, &mail(3, "billing@shop.example")).await;
    rig.store.update_emails_in_share(rig.mia.id, vec![keyword(third, "rechnungen", true)]).await.unwrap();
    assert_eq!(count(&rig, "SELECT COUNT(*) FROM label_training"), 1);
    assert_eq!(knowledge(&rig).await.senders.get(&bills), Some(&1));

    // A full queue takes no more of this account; another account's job is in the next batch.
    let leni = crate::common::account(&rig.store, "leni@example.org").await;
    {
        let db = rusqlite::Connection::open(rig.dir.path().join("uwumail.db")).unwrap();
        for email in 1000..1600 {
            db.execute(
                "INSERT INTO label_training (account_id, email_id, label_id, positive, queued_at) VALUES (?1, ?2, ?3, 1, 0)",
                rusqlite::params![rig.mia.id, email, bills],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO label_training (account_id, email_id, label_id, positive, queued_at) VALUES (?1, 1, 1, 1, 0)",
            [leni.id],
        )
        .unwrap();
    }
    rig.store.update_emails(rig.mia.id, vec![keyword(third, "rechnungen", true)]).await.unwrap();
    let mia = rig.mia.id;
    assert_eq!(count(&rig, &format!("SELECT COUNT(*) FROM label_training WHERE account_id = {mia}")), 601);
    let batch = rig.store.label_training().await.unwrap();
    assert!(batch.iter().any(|job| job.account_id == leni.id), "accounts take turns");
}
