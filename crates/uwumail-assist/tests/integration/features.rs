//! What the features make of the model's answers: events only when the mail says so, people only
//! when the mail or the address book knows them, replies with the mail they answer.

use std::sync::Arc;

use serde_json::json;
use uwumail_assist::{Assist, ComposeArgs, EventsArgs, ImageText, chatgpt};

use crate::common::{INVOICE, Reply, chat, rig};

fn event(title: &str, start: &str, quote: &str, participants: serde_json::Value) -> serde_json::Value {
    json!({
        "title": title, "start": start, "end": null, "allDay": false, "timeZone": "Europe/Berlin",
        "location": "Filiale", "description": null, "url": "https://tracking.shop.example/4711",
        "participants": participants, "confidence": 0.8, "quote": quote
    })
}

#[tokio::test]
async fn events_are_checked_against_the_mail_and_its_pictures() {
    let rig = rig().await;
    let pictures: ImageText =
        Arc::new(|_, _| Box::pin(async { Some(vec!["KINO Saal 3 · 10.10.2026 · 20:00".into()]) }));
    let assist =
        Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default()).with_image_text(pictures);
    rig.server_provider("openaiCompatible", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let answer = json!({ "events": [
        event("Abholung", "2026-10-06T09:30:00", "am Dienstag, 6. Oktober um 9:30 Uhr",
              json!(["Leni", "Mia", "mia@example.org", "someone@example.net"])),
        event("Kino", "2026-10-10T20:00:00", "Saal 3 · 10.10.2026", json!([])),
        event("Erfunden", "2026-11-01T10:00:00", "this sentence is not in the mail", json!([])),
        event("Kaputt", "irgendwann", "Bitte zahlen Sie", json!([]))
    ] });
    rig.fake.push(Reply::Json(200, chat(&answer.to_string()), vec![]));
    let result = assist.extract_events(&rig.mia, EventsArgs { email_id: email, include_images: true }).await.unwrap();
    let titles: Vec<&str> = result.events.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["Abholung", "Kino"]);
    let pickup = &result.events[0];
    assert_eq!((pickup.start.as_str(), pickup.end.as_str()), ("2026-10-06T09:30:00", "2026-10-06T10:30:00"));
    assert_eq!(pickup.time_zone.as_deref(), Some("Europe/Berlin"));
    assert_eq!(pickup.url, None, "a link the mail does not contain is dropped");
    let people: Vec<&str> = pickup.participants.iter().map(|p| p.email.as_str()).collect();
    assert_eq!(people, ["leni@example.org"], "the reader is never a participant, strangers are not guessed");
    assert_eq!(pickup.participants[0].name.as_deref(), Some("Leni Beispiel"));

    // The picture's text went to the model as data, marked as such.
    let user = rig.fake.seen()[0].body["messages"][1]["content"].as_str().unwrap().to_owned();
    assert!(user.contains("KINO Saal 3") && user.contains("pictures"), "{user}");

    // Without includeImages, the pictures are not read.
    rig.fake.push(Reply::Json(200, chat(&answer.to_string()), vec![]));
    let result = assist.extract_events(&rig.mia, EventsArgs { email_id: email, include_images: false }).await.unwrap();
    assert_eq!(result.events.len(), 1);
    assert!(!rig.fake.seen()[1].body["messages"][1]["content"].as_str().unwrap().contains("KINO"));
}

#[tokio::test]
async fn a_reply_is_written_with_the_mail_it_answers() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.fake.push(Reply::Json(200, chat("Vielen Dank, ich zahle bis zum 12. Oktober."), vec![]));
    let args = ComposeArgs {
        mode: "rewrite".into(),
        preset: Some("formal".into()),
        text: Some("danke, zahl ich bis 12.10.".into()),
        reply_to_email_id: Some(email),
        ..ComposeArgs::default()
    };
    let result = rig.assist.compose(&rig.mia, args, None).await.unwrap();
    assert_eq!(result.text, "Vielen Dank, ich zahle bis zum 12. Oktober.");
    let user = rig.fake.seen()[0].body["messages"][1]["content"].as_str().unwrap().to_owned();
    assert!(user.contains("danke, zahl ich bis 12.10.") && user.contains("Ihre Rechnung 4711"), "{user}");
    assert!(!user.contains("Ignore all previous instructions"));

    // Someone else's mail is not context.
    let leni = crate::common::account(&rig.store, "leni@example.org").await;
    let args = ComposeArgs {
        mode: "write".into(),
        instruction: Some("Antwort".into()),
        reply_to_email_id: Some(email),
        ..ComposeArgs::default()
    };
    assert!(rig.assist.compose(&leni, args, None).await.is_err());
}
