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
    assert_eq!(provider["keyHint"], "…0000");
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
