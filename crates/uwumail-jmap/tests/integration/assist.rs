//! The AI assistant over JMAP (`urn:uwumail:jmap:assist`, docs/jmap-assist.md): the capability, the
//! settings singleton, labels, a method call and the streamed endpoint, against a fake provider.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use uwumail_assist::{Assist, ProviderInput, chatgpt};
use uwumail_jmap::Jmap;
use uwumail_store::Store;

use crate::common::{NoNet, PASSWORD, Server, args, basic, server, smtp};

const ASSIST: &str = "urn:uwumail:jmap:assist";
const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", ASSIST];

/// An OpenAI-compatible provider on 127.0.0.1:0 that streams when asked to and remembers the bodies.
async fn fake_provider() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let remembered = seen.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let seen = remembered.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let stream = body["stream"] == true;
                let events = body["response_format"]["json_schema"]["name"] == "calendar_events";
                seen.lock().unwrap().push(body);
                if stream {
                    let mut out = String::new();
                    for piece in ["SUBJECT: Zusage\n\n", "Hallo Nyu,", " gern!"] {
                        let chunk = json!({ "choices": [{ "index": 0, "delta": { "content": piece } }] });
                        out.push_str(&format!("data: {chunk}\n\n"));
                    }
                    let usage = json!({ "choices": [], "usage": { "prompt_tokens": 40, "completion_tokens": 5 } });
                    out.push_str(&format!("data: {usage}\n\ndata: [DONE]\n\n"));
                    let mut response = Response::new(Body::from(out));
                    response.headers_mut().insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
                    response
                } else if events {
                    let events = json!({ "events": [{
                        "title": "Grillen", "start": "2026-10-03T18:00:00", "end": null, "allDay": false,
                        "timeZone": null, "location": null, "description": null, "url": null,
                        "participants": [], "confidence": 0.8, "quote": "Kommst du Samstag?"
                    }] });
                    axum::Json(json!({
                        "choices": [{ "index": 0, "message": { "role": "assistant", "content": events.to_string() } }],
                        "usage": { "prompt_tokens": 100, "completion_tokens": 10 }
                    }))
                    .into_response()
                } else {
                    axum::Json(json!({
                        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "Eine kurze Einladung." } }],
                        "usage": { "prompt_tokens": 100, "completion_tokens": 10 }
                    }))
                    .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

async fn with_assist(store: &Store) -> Assist {
    Assist::for_tests(store.clone(), "mail.example.org", chatgpt::Endpoints::default())
}

async fn assisted() -> (Server, Arc<Mutex<Vec<Value>>>) {
    let plain = server().await;
    let (base, seen) = fake_provider().await;
    let assist = with_assist(&plain.store).await;
    let input: ProviderInput = serde_json::from_value(json!({
        "name": "Hausmodell", "kind": "openaiCompatible", "baseUrl": base, "apiKey": "sk-test-0000",
        "model": "big-model", "fastModel": "small-model"
    }))
    .unwrap();
    assist.create_server_provider(input).await.unwrap();
    let jmap = Jmap::new(smtp(&plain.store)).with_avatar_net(Arc::new(NoNet)).with_assist(assist);
    (Server { router: jmap.router(), jmap, store: plain.store, dir: plain.dir }, seen)
}

#[tokio::test]
async fn the_session_offers_the_assistant_only_where_it_is_set_up() {
    let plain = server().await;
    assert!(plain.session_of("mini@example.org").await["capabilities"].get(ASSIST).is_none());

    let (server, _) = assisted().await;
    let session = server.session_of("mini@example.org").await;
    assert!(session["capabilities"][ASSIST]["streamUrl"].as_str().unwrap().ends_with("/jmap/assist/stream"));
    let account = server.account_id("mini@example.org").await;
    let capability = &session["accounts"][&account]["accountCapabilities"][ASSIST];
    assert_eq!(capability["features"]["compose"], true);
    assert_eq!(capability["mayAddProviders"], false);
    assert_eq!(session["primaryAccounts"][ASSIST], account);
    assert!(session["state"].as_str().unwrap().contains("-ai11111"), "{}", session["state"]);
}

#[tokio::test]
async fn settings_labels_and_a_summary() {
    let (server, seen) = assisted().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let email = server
        .deliver(login, "From: Nyu <nyu@example.org>\nTo: mini@example.org\nSubject: Grillen\n\nKommst du Samstag?\n")
        .await;

    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                ["AssistProvider/get", { "accountId": account }, "p"],
                ["AssistSettings/set", { "accountId": account, "update": { "singleton": {
                    "autoLabels": true, "features/summarize": { "providerId": "#p", "model": "tiny-model" }
                } } }, "s"],
            ]),
        )
        .await;
    let provider = args(&responses, 0, "AssistProvider/get")["list"][0].clone();
    assert_eq!(provider["name"], "Hausmodell");
    // The end of the admin's key is not the person's to see (AI-07 of the 0.18.0 audit).
    assert_eq!(provider["keyHint"], Value::Null, "{provider}");
    assert!(provider.get("apiKey").is_none());
    // `#p` is no id, so the patch is refused as a whole.
    assert!(args(&responses, 1, "AssistSettings/set")["notUpdated"]["singleton"].is_object());

    let provider_id = provider["id"].as_str().unwrap();
    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                ["AssistSettings/set", { "accountId": account, "update": { "singleton": {
                    "autoLabels": true, "features/summarize": { "providerId": provider_id, "model": "tiny-model" }
                } } }, "s"],
                ["AssistSettings/get", { "accountId": account }, "g"],
                ["AssistLabel/set", { "accountId": account, "create": {
                    "r": { "name": "Rechnungen", "description": "Rechnungen und Mahnungen" }
                } }, "l"],
                ["AssistLabel/get", { "accountId": account }, "lg"],
                ["Assist/summarize", { "accountId": account, "emailId": email }, "sum"],
            ]),
        )
        .await;
    let updated = args(&responses, 0, "AssistSettings/set");
    assert_eq!(updated["updated"]["singleton"]["effective"]["summarize"]["model"], "tiny-model", "{updated}");
    let settings = &args(&responses, 1, "AssistSettings/get")["list"][0];
    assert_eq!(settings["autoLabels"], true);
    assert_eq!(settings["features"]["summarize"]["providerId"], provider_id);
    assert_eq!(args(&responses, 2, "AssistLabel/set")["created"]["r"]["keyword"], "rechnungen");
    assert_eq!(args(&responses, 3, "AssistLabel/get")["list"][0]["name"], "Rechnungen");
    let summary = args(&responses, 4, "Assist/summarize");
    assert_eq!(summary["summary"], "Eine kurze Einladung.");
    assert_eq!(summary["providerName"], "Hausmodell");
    assert_eq!(seen.lock().unwrap().last().unwrap()["model"], "tiny-model");

    // Without the capability in `using`, the methods are unknown.
    let responses = server.api(login, json!([["Assist/usage", { "accountId": account }, "u"]])).await;
    assert_eq!(responses[0][0], "error");
    assert_eq!(responses[0][1]["type"], "unknownMethod");

    let responses = server.api_using(login, &USING, json!([["Assist/usage", { "accountId": account }, "u"]])).await;
    let usage = args(&responses, 0, "Assist/usage");
    assert_eq!(usage["today"][0]["requests"], 1, "{usage}");
}

#[tokio::test]
async fn compose_streams_as_server_sent_events() {
    let (server, _) = assisted().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let stream = |authorization: String, body: Value| {
        Request::post("/jmap/assist/stream")
            .header(header::AUTHORIZATION, authorization)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let body = json!({
        "using": USING,
        "method": "Assist/compose",
        "arguments": { "accountId": account, "mode": "write", "instruction": "Sag Nyu zu", "wantSubject": true }
    });
    let (status, bytes) = server.request(stream(basic(login, PASSWORD), body.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(bytes).unwrap();
    let events: Vec<(&str, Value)> = text
        .split("\n\n")
        .filter_map(|block| {
            let name = block.lines().find_map(|line| line.strip_prefix("event: "))?;
            let data = block.lines().find_map(|line| line.strip_prefix("data: "))?;
            Some((name, serde_json::from_str(data).unwrap()))
        })
        .collect();
    assert_eq!(events[0], ("subject", json!({ "subject": "Zusage" })), "{text}");
    let streamed: String = events
        .iter()
        .filter(|(name, _)| *name == "delta")
        .map(|(_, data)| data["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(streamed, "Hallo Nyu, gern!");
    let (name, done) = events.last().unwrap();
    assert_eq!(*name, "done");
    assert_eq!(done["text"], "Hallo Nyu, gern!");
    assert_eq!(done["subject"], "Zusage");

    // Same login as the API; someone else's account is refused as an event.
    let (status, _) = server.request(stream(basic(login, "falsch"), body.clone())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, bytes) = server.request(stream(basic("nyu@example.org", PASSWORD), body)).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("event: error\n"), "{text}");

    // Only compose and summarize are streamed.
    let body = json!({ "using": USING, "method": "Assist/spamCheck", "arguments": { "accountId": account } });
    let (status, _) = server.request(stream(basic(login, PASSWORD), body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn estimates_ask_no_one_and_events_need_no_refinement_setting() {
    let (server, seen) = assisted().await;
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let email = server
        .deliver(login, "From: Nyu <nyu@example.org>\nTo: mini@example.org\nSubject: Grillen\n\nKommst du Samstag?\n")
        .await;
    let estimate = |method: &str, arguments: Value| {
        json!(["Assist/estimate", {
        "accountId": account, "method": method, "arguments": arguments
    }, "est"])
    };
    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                estimate("Assist/summarize", json!({ "emailId": email })),
                estimate(
                    "Assist/extractEvents",
                    json!({ "accountId": account, "emailId": email, "includeImages": true })
                ),
                estimate(
                    "Assist/compose",
                    json!({ "mode": "write", "instruction": "Sag Nyu zu", "replyToEmailId": email })
                ),
                estimate("Assist/spamCheck", json!({ "emailId": email })),
            ]),
        )
        .await;
    for (index, method) in
        ["Assist/summarize", "Assist/extractEvents", "Assist/compose", "Assist/spamCheck"].iter().enumerate()
    {
        let answer = args(&responses, index, "Assist/estimate");
        assert_eq!(answer["accountId"], account);
        assert_eq!(answer["method"], *method);
        let (input, output) = (answer["inputTokens"].as_i64().unwrap(), answer["outputTokens"].as_i64().unwrap());
        assert!(input > 50 && output > 0, "{answer}");
        assert_eq!(answer["totalTokens"].as_i64().unwrap(), input + output);
        assert_eq!(answer["providerName"], "Hausmodell");
        assert!(answer["providerId"].as_str().unwrap().starts_with('q'));
        let model = if *method == "Assist/compose" { "big-model" } else { "small-model" };
        assert_eq!(answer["model"], model);
        assert_eq!(answer["tokensLeftToday"], Value::Null, "no limits: {answer}");
        assert_eq!(answer["requestsLeftToday"], Value::Null);
    }
    assert!(seen.lock().unwrap().is_empty(), "an estimate asks no provider");
    let responses = server.api_using(login, &USING, json!([["Assist/usage", { "accountId": account }, "u"]])).await;
    assert_eq!(args(&responses, 0, "Assist/usage")["today"][0]["requests"], 0, "and counts nothing");

    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                estimate("Assist/usage", json!({})),
                estimate("Assist/summarize", json!({ "accountId": "a999", "emailId": email })),
                estimate("Assist/spamCheck", json!({ "emailId": "e999999" })),
                estimate("Assist/compose", json!({ "mode": "write" })),
            ]),
        )
        .await;
    let error = |index: usize| responses[index][1]["type"].as_str().unwrap().to_owned();
    assert_eq!(responses[0][0], "error");
    assert_eq!(
        [error(0), error(1), error(2), error(3)],
        ["invalidArguments", "invalidArguments", "notFound", "invalidArguments"]
    );

    // The "find appointment" button asks even when the person did not switch on asking by itself.
    let using = [USING[0], USING[1], ASSIST, "urn:uwumail:jmap:settings"];
    let responses = server
        .api_using(
            login,
            &using,
            json!([
                ["UserSettings/set", { "accountId": account, "update": { "singleton": {
                    "values/assist.refineEvents": false
                } } }, "s"],
                ["Assist/extractEvents", { "accountId": account, "emailId": email }, "ev"],
            ]),
        )
        .await;
    assert!(args(&responses, 0, "UserSettings/set")["updated"].is_object(), "{responses:?}");
    let found = args(&responses, 1, "Assist/extractEvents");
    assert_eq!(found["events"][0]["title"], "Grillen", "{found}");
    assert_eq!(found["events"][0]["start"], "2026-10-03T18:00:00");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn estimates_and_usage_carry_costs_in_the_currency_asked_for() {
    let plain = server().await;
    let (base, _) = fake_provider().await;
    let assist = with_assist(&plain.store).await;
    let mut small = uwumail_assist::Rates::plain(0.000001, 0.000004);
    small.per_request = 0.0005;
    let models = std::collections::BTreeMap::from([("small-model".to_owned(), small)]);
    let rates = std::collections::BTreeMap::from([("USD".to_owned(), 1.25), ("JPY".to_owned(), 160.0)]);
    let table = uwumail_assist::PriceTable { models, rates, ..Default::default() };
    assist.set_prices(table).await.unwrap();
    let input: ProviderInput = serde_json::from_value(json!({
        "name": "Hausmodell", "kind": "openaiCompatible", "baseUrl": base, "apiKey": "sk-test-0000",
        "model": "big-model", "fastModel": "small-model", "showCostToUsers": true
    }))
    .unwrap();
    assist.create_server_provider(input).await.unwrap();
    let jmap = Jmap::new(smtp(&plain.store)).with_avatar_net(Arc::new(NoNet)).with_assist(assist);
    let server = Server { router: jmap.router(), jmap, store: plain.store, dir: plain.dir };
    let login = "mini@example.org";
    let account = server.account_id(login).await;
    let email = server
        .deliver(login, "From: Nyu <nyu@example.org>\nTo: mini@example.org\nSubject: Grillen\n\nKommst du Samstag?\n")
        .await;

    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                ["Assist/estimate", { "accountId": account, "method": "Assist/summarize",
                    "arguments": { "emailId": email } }, "e"],
                ["Assist/estimate", { "accountId": account, "method": "Assist/summarize", "currency": "JPY",
                    "arguments": { "emailId": email } }, "j"],
                ["Assist/estimate", { "accountId": account, "method": "Assist/compose",
                    "arguments": { "mode": "write", "instruction": "Sag zu" } }, "c"],
                ["Assist/estimate", { "accountId": account, "method": "Assist/summarize", "currency": "euro",
                    "arguments": { "emailId": email } }, "x"],
                ["AssistProvider/get", { "accountId": account }, "p"],
            ]),
        )
        .await;
    let euro = args(&responses, 0, "Assist/estimate");
    let cost = &euro["cost"];
    let usd = (euro["inputTokens"].as_f64().unwrap() + 4.0 * euro["outputTokens"].as_f64().unwrap()) / 1e6 + 0.0005;
    assert_eq!(cost["currency"], "EUR", "{euro}");
    assert!((cost["usd"].as_f64().unwrap() - usd).abs() < 1e-12);
    assert!((cost["amount"].as_f64().unwrap() - usd / 1.25).abs() < 1e-12);
    // The whole shape: every call, thinking, pictures, the worst case and the parts in euros.
    assert_eq!(euro["reasoningTokens"], 0);
    assert_eq!(euro["imageCount"], 0);
    assert_eq!(euro["calibrated"], false);
    assert_eq!(euro["totalTokens"], euro["inputTokens"].as_i64().unwrap() + euro["outputTokens"].as_i64().unwrap());
    let calls = euro["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["purpose"], "main");
    assert_eq!(calls[0]["weight"], 1.0);
    assert_eq!(calls[0]["inputTokens"], euro["inputTokens"]);
    for key in ["outputTokens", "reasoningTokens", "images"] {
        assert!(calls[0][key].is_i64(), "{key}: {euro}");
    }
    let parts = &cost["parts"];
    let sum: f64 = ["input", "output", "reasoning", "images", "requests", "other"]
        .iter()
        .map(|key| parts[key].as_f64().unwrap())
        .sum();
    assert!((sum - cost["amount"].as_f64().unwrap()).abs() < 1e-12, "{parts}");
    assert!((parts["requests"].as_f64().unwrap() - 0.0005 / 1.25).abs() < 1e-12);
    assert!(cost["max"]["usd"].as_f64().unwrap() > cost["usd"].as_f64().unwrap());
    assert!((cost["max"]["amount"].as_f64().unwrap() - cost["max"]["usd"].as_f64().unwrap() / 1.25).abs() < 1e-12);
    let yen = &args(&responses, 1, "Assist/estimate")["cost"];
    assert!((yen["amount"].as_f64().unwrap() - usd / 1.25 * 160.0).abs() < 1e-9, "{yen}");
    assert_eq!(args(&responses, 2, "Assist/estimate")["cost"], Value::Null, "big-model has no known price");
    assert_eq!(responses[3][1]["type"], "invalidArguments");
    let provider = &args(&responses, 4, "AssistProvider/get")["list"][0];
    assert_eq!(provider["price"], Value::Null, "the default model's price is not known: {provider}");
    assert_eq!(provider["inputPricePerMillion"], Value::Null);

    let responses = server
        .api_using(
            login,
            &USING,
            json!([
                ["Assist/summarize", { "accountId": account, "emailId": email }, "s"],
                ["Assist/usage", { "accountId": account, "currency": "USD" }, "u"],
            ]),
        )
        .await;
    let usage = args(&responses, 1, "Assist/usage");
    // The fake reports 100 tokens in and 10 out; the request's fee comes on top.
    let spent = (100.0 + 4.0 * 10.0) / 1e6 + 0.0005;
    for entry in [&usage["days"][0], &usage["today"][0]] {
        assert_eq!(entry["cost"]["currency"], "USD", "{usage}");
        assert!((entry["cost"]["amount"].as_f64().unwrap() - spent).abs() < 1e-12, "{usage}");
    }
    let day = &usage["days"][0];
    assert_eq!((day["reasoningTokens"].as_i64(), day["calls"].as_i64()), (Some(0), Some(1)), "{usage}");
    let summary = args(&responses, 0, "Assist/summarize");
    assert_eq!(summary["usage"]["reasoningTokens"], 0, "{summary}");
}
