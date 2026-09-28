//! The OAuth scope `maskedemail` (docs/jmap-masked-email.md): an app such as UwULock Server makes
//! masked addresses for a person and sees nothing else of the mailbox.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_store::{AccountMaskedPolicy, DomainKind, DomainMaskedPolicy, MaskedMode, NewOAuthCode, OAuthTokens};

use crate::common::{Server, USING, args, basic, server};
use crate::websocket::{connect, listen, receive, send};

const MASKED: &str = "https://www.fastmail.com/dev/maskedemail";
const CORE: &str = "urn:ietf:params:jmap:core";
const MASKED_USING: [&str; 2] = [CORE, MASKED];
const LOCK: &str = "UwULock (lock.example.com)";
const REDIRECT: &str = "https://lock.example.com/uwu/v1/masked/callback";
/// RFC 7636 appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Signs `mini` in to an app named `name` with these scopes, as the consent page and the token
/// endpoint would.
async fn sign_in(server: &Server, name: &str, scopes: Vec<&'static str>) -> OAuthTokens {
    let store = &server.store;
    let client = store.register_oauth_client(name, vec![REDIRECT.into()]).await.unwrap();
    let code = store
        .create_oauth_code(NewOAuthCode {
            client_id: client.id,
            account_id: server.id("mini@example.org").await,
            redirect_uri: REDIRECT.into(),
            scopes,
            code_challenge: CHALLENGE.into(),
            nonce: None,
            auth_time: 0,
        })
        .await
        .unwrap();
    store.redeem_oauth_code(&code, client.id, REDIRECT, VERIFIER).await.unwrap().unwrap()
}

async fn get(server: &Server, path: &str, authorization: &str) -> (StatusCode, Value) {
    let request = Request::get(path)
        .header(header::AUTHORIZATION, authorization)
        .header(header::HOST, "mail.example.org")
        .body(Body::empty())
        .unwrap();
    let (status, body) = server.request(request).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[tokio::test(flavor = "multi_thread")]
async fn a_masked_only_app_sees_masked_addresses_and_nothing_else() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    server.store.create_domain_with_kind("masked.test", DomainKind::Masked).await.unwrap();
    let policy = DomainMaskedPolicy {
        mode: MaskedMode::Dedicated,
        masked_domains: vec!["masked.test".into()],
        default_domain: None,
    };
    server.store.set_domain_masked_policy("example.org", policy).await.unwrap();
    // Nyu shares the inbox with Mini: the app does not see that account either.
    let nyu = server.account_id("nyu@example.org").await;
    let inbox = server.mailbox("nyu@example.org", "inbox").await;
    let principal = format!("p{}", server.id("mini@example.org").await);
    let shared = server
        .api_using(
            "nyu@example.org",
            &[USING.as_slice(), &["urn:ietf:params:jmap:principals"]].concat(),
            json!([["Mailbox/set", { "accountId": nyu, "update": {
                (inbox.clone()): { format!("shareWith/{principal}"): { "mayReadItems": true } }
            } }, "0"]]),
        )
        .await;
    assert!(args(&shared, 0, "Mailbox/set")["updated"].get(&inbox).is_some(), "{shared:?}");
    let mail = server.deliver("mini@example.org", "From: a@example.net\nSubject: Geheim\n\nnur fuer mich\n").await;

    let lock = sign_in(&server, LOCK, vec!["maskedemail"]).await;
    let token = bearer(&lock.access_token);

    // The session has the core and MaskedEmail, one account with MaskedEmail alone.
    let (status, session) = get(&server, "/jmap/session", &token).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(keys(&session["capabilities"]), vec![MASKED.to_owned(), CORE.to_owned()]);
    assert_eq!(keys(&session["accounts"]), vec![account.clone()], "no shared account");
    assert_eq!(
        session["accounts"][&account]["accountCapabilities"],
        json!({ MASKED: { "domains": ["masked.test"], "defaultDomain": "masked.test" } })
    );
    assert_eq!(session["primaryAccounts"], json!({ MASKED: account }));
    assert_eq!(session["username"], "mini@example.org");
    let (_, well_known) = get(&server, "/.well-known/jmap", &token).await;
    assert_eq!(well_known["capabilities"], session["capabilities"]);

    // Core/echo and MaskedEmail work, and what it makes says who made it.
    let (status, response) = server
        .api_as(
            &token,
            &MASKED_USING,
            json!([
                ["Core/echo", { "hello": true }, "0"],
                ["MaskedEmail/set", { "accountId": account, "create": {
                    "k": { "state": "enabled", "forDomain": "https://shop.example.com", "description": "Shop" },
                    "own": { "domain": "example.org" }
                } }, "1"],
                ["MaskedEmail/get", { "accountId": account, "ids": ["#k"] }, "2"],
                ["MaskedEmail/changes", { "accountId": account, "sinceState": "0" }, "3"]
            ]),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let responses = response["methodResponses"].as_array().unwrap();
    assert_eq!(args(responses, 0, "Core/echo")["hello"], true);
    let set = args(responses, 1, "MaskedEmail/set");
    let email = set["created"]["k"]["email"].as_str().unwrap().to_owned();
    assert!(email.ends_with("@masked.test"), "{set}");
    assert_eq!(set["created"]["k"]["createdBy"], format!("OAuth:{LOCK}"));
    // The masked address policy holds for the app as for anyone: not on the own domain here.
    assert_eq!(set["notCreated"]["own"]["type"], "forbidden", "{set}");
    assert_eq!(args(responses, 2, "MaskedEmail/get")["list"][0]["createdBy"], format!("OAuth:{LOCK}"));
    let id = set["created"]["k"]["id"].as_str().unwrap().to_owned();
    assert!(args(responses, 3, "MaskedEmail/changes")["created"].as_array().unwrap().contains(&json!(id)));
    assert_eq!(response["sessionState"], session["state"]);

    // Anything else is forbidden, in any account, whatever `using` says.
    let using = [USING.as_slice(), &[MASKED, "urn:ietf:params:jmap:calendars", "urn:ietf:params:jmap:sieve"]].concat();
    let refused = [
        json!(["Mailbox/get", { "accountId": account }, "0"]),
        json!(["Email/get", { "accountId": account, "ids": [mail] }, "1"]),
        json!(["Email/query", { "accountId": account }, "2"]),
        json!(["Identity/get", { "accountId": account }, "3"]),
        json!(["EmailSubmission/set", { "accountId": account, "create": {} }, "4"]),
        json!(["UserSettings/get", { "accountId": account }, "5"]),
        json!(["Calendar/get", { "accountId": account }, "6"]),
        json!(["SieveScript/get", { "accountId": account }, "7"]),
        json!(["PushSubscription/set", { "create": {} }, "8"]),
        json!(["Mailbox/get", { "accountId": nyu }, "9"]),
        json!(["MaskedEmail/get", { "accountId": nyu }, "10"]),
    ];
    let (status, response) = server.api_as(&token, &using, Value::Array(refused.to_vec())).await;
    assert_eq!(status, StatusCode::OK);
    let responses = response["methodResponses"].as_array().unwrap();
    for (index, answer) in responses.iter().enumerate().take(10) {
        assert_eq!((answer[0].as_str(), answer[1]["type"].as_str()), (Some("error"), Some("forbidden")), "{index}");
    }
    assert_eq!(responses[10][1]["type"], "accountNotFound", "{}", responses[10]);

    // Uploads, downloads and the picture proxy refuse the token outright.
    let upload = Request::post(format!("/jmap/upload/{account}/"))
        .header(header::AUTHORIZATION, &token)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("hallo"))
        .unwrap();
    assert_eq!(server.request(upload).await.0, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&server, &format!("/jmap/download/{account}/b1/x?accept=text/plain"), &token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&server, &format!("/jmap/picture/{account}?email=a%40example.net"), &token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An admin turning masked addresses off for Mini holds for the app too.
    let off = AccountMaskedPolicy { mode: Some(MaskedMode::Off), ..Default::default() };
    server.store.set_account_masked_policy(server.id("mini@example.org").await, off).await.unwrap();
    let (_, response) = server
        .api_as(
            &token,
            &MASKED_USING,
            json!([["MaskedEmail/set", { "accountId": account, "create": { "k": {} } }, "0"]]),
        )
        .await;
    assert_eq!(response["methodResponses"][0][1]["notCreated"]["k"]["type"], "forbidden", "{response}");

    // Signed out under Security, the token stops working.
    server.store.revoke_oauth_grant(server.id("mini@example.org").await, lock.grant_id).await.unwrap();
    assert_eq!(get(&server, "/jmap/session", &token).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn mail_apps_keep_everything_and_name_themselves_too() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    server
        .store
        .set_domain_masked_policy("example.org", DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() })
        .await
        .unwrap();
    let app = sign_in(&server, "Thunderbird", vec!["mail", "maskedemail"]).await;
    let token = bearer(&app.access_token);
    let (_, session) = get(&server, "/jmap/session", &token).await;
    assert!(session["capabilities"].get("urn:ietf:params:jmap:mail").is_some(), "{session}");
    let using = [USING.as_slice(), &[MASKED]].concat();
    let (_, response) = server
        .api_as(
            &token,
            &using,
            json!([
                ["Mailbox/get", { "accountId": account }, "0"],
                ["MaskedEmail/set", { "accountId": account, "create": { "k": {} } }, "1"]
            ]),
        )
        .await;
    let responses = response["methodResponses"].as_array().unwrap();
    assert_eq!(responses[0][0], "Mailbox/get");
    assert_eq!(args(responses, 1, "MaskedEmail/set")["created"]["k"]["createdBy"], "OAuth:Thunderbird");
    // The account password is no OAuth app.
    let responses = server
        .api_using(
            "mini@example.org",
            &using,
            json!([["MaskedEmail/set", { "accountId": account, "create": { "k": {} } }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "MaskedEmail/set")["created"]["k"]["createdBy"], "JMAP");
    // A token for sending only gets no masked addresses.
    let smtp = sign_in(&server, "Sender", vec!["smtp"]).await;
    assert_eq!(get(&server, "/jmap/session", &bearer(&smtp.access_token)).await.0, StatusCode::UNAUTHORIZED);
}

/// Push over the event stream and the WebSocket tells a masked-only app about masked addresses and
/// nothing else, whatever types it asks for.
#[tokio::test(flavor = "multi_thread")]
async fn push_tells_a_masked_only_app_about_masked_addresses_only() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    server
        .store
        .set_domain_masked_policy("example.org", DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() })
        .await
        .unwrap();
    let lock = sign_in(&server, LOCK, vec!["maskedemail"]).await;
    let token = bearer(&lock.access_token);
    let create = |key: &str| json!([["MaskedEmail/set", { "accountId": account, "create": { (key): {} } }, "0"]]);

    // The event stream: mail arrives first, then a masked address is made; only that is told.
    let request = Request::get("/jmap/eventsource/?types=*&closeafter=state&ping=0")
        .header(header::AUTHORIZATION, &token)
        .body(Body::empty())
        .unwrap();
    let response = server.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stream =
        tokio::spawn(async move { String::from_utf8(to_bytes(response.into_body(), 1 << 20).await.unwrap().to_vec()) });
    server.deliver("mini@example.org", "From: a@example.net\nSubject: Hallo\n\nhallo\n").await;
    let responses = server.api_using("mini@example.org", &MASKED_USING, create("a")).await;
    let state = args(&responses, 0, "MaskedEmail/set")["newState"].clone();
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), stream).await.unwrap().unwrap().unwrap();
    let data = event.lines().find_map(|line| line.strip_prefix("data:")).unwrap();
    let data: Value = serde_json::from_str(data.trim()).unwrap();
    assert_eq!(data["changed"], json!({ (account.clone()): { "MaskedEmail": state } }), "{event}");

    // The WebSocket: requests are kept to MaskedEmail, and so is push.
    let url = listen(server.router.clone()).await;
    assert!(connect(&url, Some(&basic("mini@example.org", "nope")), Some("jmap")).await.is_err());
    let mut socket = connect(&url, Some(&token), Some("jmap")).await.unwrap();
    let using = [USING.as_slice(), &[MASKED]].concat();
    let request = json!({
        "@type": "Request",
        "id": "r1",
        "using": using,
        "methodCalls": [["Mailbox/get", { "accountId": account }, "0"], ["MaskedEmail/get", { "accountId": account }, "1"]]
    });
    send(&mut socket, request).await;
    let response = receive(&mut socket).await;
    assert_eq!(response["methodResponses"][0][1]["type"], "forbidden", "{response}");
    assert_eq!(response["methodResponses"][1][0], "MaskedEmail/get", "{response}");
    send(&mut socket, json!({ "@type": "WebSocketPushEnable", "dataTypes": ["Email", "Mailbox", "MaskedEmail"] }))
        .await;
    // Answered once the push is on: from here on no change slips past.
    send(
        &mut socket,
        json!({ "@type": "Request", "id": "r2", "using": MASKED_USING, "methodCalls": [["Core/echo", {}, "0"]] }),
    )
    .await;
    assert_eq!(receive(&mut socket).await["requestId"], "r2");
    server.deliver("mini@example.org", "From: a@example.net\nSubject: Noch mal\n\nhallo\n").await;
    let responses = server.api_using("mini@example.org", &MASKED_USING, create("b")).await;
    let state = args(&responses, 0, "MaskedEmail/set")["newState"].clone();
    let pushed = receive(&mut socket).await;
    assert_eq!(pushed["@type"], "StateChange");
    assert_eq!(pushed["changed"], json!({ (account.clone()): { "MaskedEmail": state } }), "{pushed}");
}
