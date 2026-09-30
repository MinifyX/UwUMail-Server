//! `Assist/estimate`: the prompt the call would send, counted like the call counts it, with nothing
//! sent, nothing counted against the day's limits, and pictures never read for it.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uwumail_assist::llm::estimate_texts;
use uwumail_assist::{
    Assist, AssistError, ComposeArgs, EstimateArgs, EventsArgs, ImageText, PictureRead, PictureTexts, SpamArgs,
    SummarizeArgs, chatgpt,
};

use crate::common::{INVOICE, Reply, chat, rig};

/// The input tokens of the request the fake provider got, counted like the estimate.
fn sent_tokens(body: &Value) -> i64 {
    let schema = body.pointer("/response_format/json_schema/schema").map(Value::to_string).unwrap_or_default();
    estimate_texts([
        body["messages"][0]["content"].as_str().unwrap(),
        body["messages"][1]["content"].as_str().unwrap(),
        schema.as_str(),
    ])
}

#[tokio::test]
async fn an_estimate_is_the_prompt_the_call_sends_and_costs_nothing() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({ "requestsPerDay": 10, "tokensPerDay": 100000 })).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let assist = &rig.assist;

    let calls: Vec<(EstimateArgs, Value)> = vec![
        (
            EstimateArgs::Summarize(SummarizeArgs { email_id: Some(email), ..Default::default() }),
            chat("Eine Rechnung über 42 Euro."),
        ),
        (
            EstimateArgs::SpamCheck(SpamArgs { email_id: email, language: Some("de".into()) }),
            chat(r#"{"verdict": "legitimate", "confidence": 0.9, "reasons": ["bekannt"]}"#),
        ),
        (EstimateArgs::ExtractEvents(EventsArgs { email_id: email, include_images: false }), chat(r#"{"events": []}"#)),
        (
            EstimateArgs::Compose(ComposeArgs {
                mode: "rewrite".into(),
                preset: Some("formal".into()),
                text: Some("danke, zahl ich bis 12.10.".into()),
                reply_to_email_id: Some(email),
                ..Default::default()
            }),
            chat("Vielen Dank, ich zahle bis zum 12. Oktober."),
        ),
    ];
    for (index, (args, answer)) in calls.into_iter().enumerate() {
        let estimate = assist.estimate(&rig.mia, args.clone()).await.unwrap();
        assert!(rig.fake.seen().len() == index, "an estimate asks no one");
        // Nothing of the estimates is counted: what is left is what the real calls left.
        assert_eq!(estimate.requests_left_today, Some(10 - index as i64));
        assert_eq!(estimate.effective.provider_name, "Fake openaiCompatible");
        let expected_model = if matches!(args, EstimateArgs::Compose(_)) { "big-model" } else { "small-model" };
        assert_eq!(estimate.effective.model, expected_model);
        assert!(estimate.output_tokens > 0);

        rig.fake.push(Reply::Json(200, answer, vec![]));
        match args {
            EstimateArgs::Compose(args) => drop(assist.compose(&rig.mia, args, None).await.unwrap()),
            EstimateArgs::Summarize(args) => drop(assist.summarize(&rig.mia, args, None).await.unwrap()),
            EstimateArgs::SpamCheck(args) => drop(assist.spam_check(&rig.mia, args).await.unwrap()),
            EstimateArgs::ExtractEvents(args) => drop(assist.extract_events(&rig.mia, args).await.unwrap()),
        }
        let body = rig.fake.seen()[index].body.clone();
        assert_eq!(estimate.input_tokens, sent_tokens(&body), "call {index}: {body}");
        assert!(estimate.output_tokens <= body["max_tokens"].as_i64().unwrap());
    }
    let (_, today) = assist.usage(&rig.mia, 1).await.unwrap();
    assert_eq!(today[0].requests, 4, "the four calls, not the estimates");
    let left =
        assist.estimate(&rig.mia, EstimateArgs::SpamCheck(SpamArgs { email_id: email, language: None })).await.unwrap();
    assert_eq!(left.requests_left_today, Some(6));
    // The fake reported 150 tokens a call.
    assert_eq!(left.tokens_left_today, Some(100000 - 4 * 150));
}

#[tokio::test]
async fn estimates_are_checked_like_the_calls() {
    let rig = rig().await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let summarize = || EstimateArgs::Summarize(SummarizeArgs { email_id: Some(email), ..Default::default() });
    let refused = rig.assist.estimate(&rig.mia, summarize()).await;
    assert!(matches!(refused, Err(AssistError::Unavailable(_))), "no provider: {refused:?}");

    rig.server_provider("openaiCompatible", json!({})).await;
    let estimate = rig.assist.estimate(&rig.mia, summarize()).await.unwrap();
    assert_eq!((estimate.requests_left_today, estimate.tokens_left_today), (None, None), "no limits");

    let missing = EstimateArgs::SpamCheck(SpamArgs { email_id: email + 1000, language: None });
    assert!(matches!(rig.assist.estimate(&rig.mia, missing).await, Err(AssistError::NotFound(_))));
    let leni = crate::common::account(&rig.store, "leni@example.org").await;
    assert!(matches!(rig.assist.estimate(&leni, summarize()).await, Err(AssistError::NotFound(_))), "not hers");
    let bad = EstimateArgs::Compose(ComposeArgs { mode: "write".into(), ..Default::default() });
    assert!(matches!(rig.assist.estimate(&rig.mia, bad).await, Err(AssistError::Invalid { .. })));

    rig.policy(|policy| policy.features.set("summarize", false)).await;
    assert!(matches!(rig.assist.estimate(&rig.mia, summarize()).await, Err(AssistError::Unavailable(_))));

    // A used-up limit is no error for an estimate: nothing is left.
    rig.server_provider("openaiCompatible", json!({ "requestsPerDay": 0, "name": "Leer" })).await;
    rig.policy(|policy| policy.features.set("summarize", true)).await;
    let (_, today) = rig.assist.usage(&rig.mia, 1).await.unwrap();
    let empty = today.iter().find(|row| row.provider_name == "Leer").unwrap().provider_id;
    let settings = uwumail_assist::SettingsPatch {
        default: Some(Some(uwumail_assist::Choice { provider_id: empty, model: None })),
        ..Default::default()
    };
    rig.assist.set_settings(&rig.mia, settings).await.unwrap();
    let estimate = rig.assist.estimate(&rig.mia, summarize()).await.unwrap();
    assert_eq!((estimate.effective.provider_name.as_str(), estimate.requests_left_today), ("Leer", Some(0)));
    assert!(rig.fake.seen().is_empty());
}

#[tokio::test]
async fn pictures_are_not_read_for_an_estimate() {
    let rig = rig().await;
    let asked: Arc<Mutex<Vec<PictureRead>>> = Arc::default();
    let log = asked.clone();
    let pictures: ImageText = Arc::new(move |_, _, how| {
        log.lock().unwrap().push(how);
        Box::pin(async move {
            Some(match how {
                PictureRead::Read => PictureTexts::read(vec!["KINO Saal 3".into(), "Ticket".into(), "Plan".into()]),
                PictureRead::KnownOnly => PictureTexts { texts: vec!["KINO Saal 3".into()], unread: 2 },
            })
        })
    });
    let assist =
        Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default()).with_image_text(pictures);
    rig.server_provider("openaiCompatible", json!({})).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let events = |include_images| EstimateArgs::ExtractEvents(EventsArgs { email_id: email, include_images });

    let without = assist.estimate(&rig.mia, events(false)).await.unwrap();
    assert!(asked.lock().unwrap().is_empty(), "no pictures without includeImages");
    let with = assist.estimate(&rig.mia, events(true)).await.unwrap();
    assert_eq!(*asked.lock().unwrap(), [PictureRead::KnownOnly]);
    // The known text, and about a hundred tokens for each picture never read.
    assert!(with.input_tokens >= without.input_tokens + 200, "{} {}", with.input_tokens, without.input_tokens);
    assert!(with.input_tokens < without.input_tokens + 300, "{} {}", with.input_tokens, without.input_tokens);

    rig.fake.push(Reply::Json(200, chat(r#"{"events": []}"#), vec![]));
    assist.extract_events(&rig.mia, EventsArgs { email_id: email, include_images: true }).await.unwrap();
    assert_eq!(*asked.lock().unwrap(), [PictureRead::KnownOnly, PictureRead::Read]);
}
