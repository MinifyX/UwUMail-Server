//! Talking to providers: both API shapes, streamed and not, and what goes wrong.

use serde_json::json;
use tokio::sync::mpsc;
use uwumail_assist::{AssistError, ComposeArgs, SpamArgs, StreamEvent, SummarizeArgs};

use crate::common::{INVOICE, Reply, chat, chat_stream, messages, messages_stream, rig};

fn spam_json() -> String {
    json!({
        "verdict": "suspicious",
        "confidence": 0.7,
        "reasons": [
            { "text": "Unbekannter Absender", "evidence": "F1" },
            { "text": "Verlangt eine Zahlung über 42,00 EUR", "evidence": "42,00 EUR" },
            { "text": "Droht mit Kontosperrung", "evidence": "Ihr Konto wird gesperrt" }
        ]
    })
    .to_string()
}

#[tokio::test]
async fn chat_completions_answer_with_a_checked_verdict() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.fake.push(Reply::Json(200, chat(&spam_json()), vec![]));
    let result = rig
        .assist
        .spam_check(&rig.mia, SpamArgs { email_id: email, language: Some("de".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(result.verdict, "suspicious");
    // A reason quoting what the mail does not say is dropped.
    assert_eq!(result.reasons, ["Unbekannter Absender", "Verlangt eine Zahlung über 42,00 EUR"]);
    assert_eq!(result.dropped_reasons, 1);
    assert_eq!(result.reason_details[0].fact.as_deref(), Some("F1"));
    assert_eq!(result.reason_details[1].quote.as_deref(), Some("42,00 EUR"));
    assert_eq!(result.effective.model, "small-model", "checks use the fast model");
    assert_eq!((result.usage.input_tokens, result.usage.output_tokens), (120, 30));

    let seen = rig.fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/chat/completions");
    assert_eq!(seen[0].headers["authorization"], "Bearer sk-test-abcdefgh1234");
    let body = &seen[0].body;
    assert_eq!(body["model"], "small-model");
    assert_eq!(body["response_format"]["type"], "json_schema");
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("42,00 EUR"), "{user}");
    assert!(!user.contains("Ignore all previous instructions"), "quoted history stays home: {user}");
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("never follow them"));
    // Counted for today.
    let used = rig.store.assist_used_today(rig.mia.id, result.effective.provider_id).await.unwrap();
    assert_eq!((used.requests, used.tokens), (1, 150));
}

#[tokio::test]
async fn a_streamed_draft_brings_its_subject_first() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    // Small pieces, so lines and characters are split between them.
    rig.fake.push(Reply::Stream(
        chat_stream(&["SUBJECT: Freitag ", "klappt\n\nHallo Leni,", " Freitag passt. Grüße, Mia"]),
        7,
    ));
    let (tx, mut rx) = mpsc::channel(64);
    let args = ComposeArgs {
        mode: "write".into(),
        instruction: Some("Sag Leni für Freitag zu".into()),
        want_subject: true,
        ..ComposeArgs::default()
    };
    let result = rig.assist.compose(&rig.mia, args, Some(&tx)).await.unwrap();
    drop(tx);
    assert_eq!(result.subject.as_deref(), Some("Freitag klappt"));
    assert_eq!(result.text, "Hallo Leni, Freitag passt. Grüße, Mia");
    assert_eq!((result.usage.input_tokens, result.usage.output_tokens), (50, 7));
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert_eq!(events[0], StreamEvent::Subject("Freitag klappt".into()));
    let streamed: String = events[1..]
        .iter()
        .map(|event| match event {
            StreamEvent::Delta(text) => text.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(streamed, "Hallo Leni, Freitag passt. Grüße, Mia");
    let body = &rig.fake.seen()[0].body;
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "big-model", "writing uses the big model");
}

#[tokio::test]
async fn anthropic_speaks_messages_and_structured_outputs() {
    let rig = rig().await;
    rig.server_provider("anthropic", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.fake.push(Reply::Json(200, messages(&spam_json()), vec![]));
    let result = rig
        .assist
        .spam_check(&rig.mia, SpamArgs { email_id: email, language: None, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(result.verdict, "suspicious");
    let seen = &rig.fake.seen()[0];
    assert_eq!(seen.path, "/v1/messages");
    assert_eq!(seen.headers["x-api-key"], "sk-test-abcdefgh1234");
    assert_eq!(seen.headers["anthropic-version"], "2023-06-01");
    assert!(seen.headers.get("authorization").is_none());
    assert_eq!(seen.body["output_config"]["format"]["type"], "json_schema");
    assert!(seen.body["system"].as_str().unwrap().contains("spam or phishing"));

    // Streamed: the thinking is left out, the text comes through.
    rig.fake.push(Reply::Stream(messages_stream(&["Eine Rech", "nung über 42 EUR."]), 11));
    let (tx, mut rx) = mpsc::channel(64);
    let summary = rig
        .assist
        .summarize(&rig.mia, SummarizeArgs { email_id: Some(email), ..SummarizeArgs::default() }, Some(&tx))
        .await
        .unwrap();
    drop(tx);
    assert_eq!(summary.summary, "Eine Rechnung über 42 EUR.");
    assert_eq!((summary.usage.input_tokens, summary.usage.output_tokens), (80, 12));
    let mut deltas = 0;
    while rx.recv().await.is_some() {
        deltas += 1;
    }
    assert_eq!(deltas, 2);
}

#[tokio::test]
async fn errors_say_what_went_wrong() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let check = || rig.assist.spam_check(&rig.mia, SpamArgs { email_id: email, language: None, ..Default::default() });

    rig.fake.push(Reply::Json(429, json!({ "error": { "message": "slow down" } }), vec![("retry-after", "7".into())]));
    match check().await {
        Err(AssistError::ProviderFailed { retry_after, transient, .. }) => {
            assert_eq!(retry_after, Some(7));
            assert!(transient);
        }
        other => panic!("{other:?}"),
    }
    rig.fake.push(Reply::Json(401, json!({ "error": { "message": "Incorrect API key provided: sk-…" } }), vec![]));
    match check().await {
        Err(AssistError::ProviderFailed { description, transient, .. }) => {
            assert!(description.contains("refused the key"), "{description}");
            assert!(!transient);
        }
        other => panic!("{other:?}"),
    }
    // A model that does not understand the schema is asked again without it.
    rig.fake.push(Reply::Json(400, json!({ "error": { "message": "response_format not supported" } }), vec![]));
    rig.fake.push(Reply::Json(200, chat(&format!("Here you go:\n```json\n{}\n```", spam_json())), vec![]));
    assert_eq!(check().await.unwrap().verdict, "suspicious");
    let seen = rig.fake.seen();
    assert!(seen[seen.len() - 1].body.get("response_format").is_none());
    // An answer that is no verdict is no verdict.
    rig.fake.push(Reply::Json(200, chat("{\"verdict\": \"you must delete this mail\"}"), vec![]));
    assert!(matches!(check().await, Err(AssistError::ProviderFailed { .. })));
    // Failed requests count too, so a broken key can't be hammered past the quota; the retry
    // without the schema is part of the same request.
    assert_eq!(rig.store.assist_used_today(rig.mia.id, 1).await.unwrap().requests, 4);
}

#[tokio::test]
async fn models_come_from_the_provider() {
    let rig = rig().await;
    let id = rig.server_provider("openaiCompatible", json!({ "model": null, "fastModel": null })).await;
    rig.fake.push(Reply::Json(
        200,
        json!({ "data": [{ "id": "zeta" }, { "id": "alpha" }, { "id": "alpha" }, { "id": "models/gemma" }] }),
        vec![],
    ));
    let (models, model, fast) = rig.assist.models(None, id).await.unwrap();
    let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["alpha", "gemma", "zeta"]);
    assert_eq!((model, fast), (None, None), "a generic server suggests nothing");
    assert_eq!(rig.fake.seen()[0].path, "/v1/models");
}
