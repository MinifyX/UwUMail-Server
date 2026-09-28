//! App passwords as JMAP bearer tokens, and the token endpoint that makes them.

use crate::common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{PASSWORD, USING, basic, server};
use serde_json::{Value, json};
use uwumail_store::{AppScope, NewAppPassword};

async fn token_request(server: &common::Server, body: Value) -> (StatusCode, Value) {
    let request = Request::post("/jmap/token")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "mail.example.org")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, bytes) = server.request(request).await;
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_password_works_as_basic_password_and_as_bearer_token_everywhere() {
    let server = server().await;
    let id = server.id("mini@example.org").await;
    let account = server.account_id("mini@example.org").await;
    let created = server
        .store
        .create_app_password(
            id,
            NewAppPassword { name: "Laptop".into(), scopes: vec![AppScope::Mail], expires_at: None },
        )
        .await
        .unwrap();

    // Basic with the app password, as before.
    let calls = json!([["Core/echo", { "hello": true }, "0"]]);
    let (status, _) = server.api_as(&basic("mini@example.org", &created.secret), &USING, calls.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // Bearer: the session, the API, upload, download and the EventSource.
    let session = Request::get("/jmap/session")
        .header(header::AUTHORIZATION, bearer(&created.secret))
        .header(header::HOST, "mail.example.org")
        .body(Body::empty())
        .unwrap();
    let (status, body) = server.request(session).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(session["username"], "mini@example.org");
    assert_eq!(session["apiUrl"], "http://mail.example.org/jmap/api");
    assert_eq!(session["capabilities"]["urn:ietf:params:jmap:websocket"]["url"], "ws://mail.example.org/jmap/ws");

    let (status, response) = server.api_as(&bearer(&created.secret), &USING, calls.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["methodResponses"][0][1]["hello"], true);
    // Without the dashes and in capitals it is the same password.
    let squashed = created.secret.replace('-', "").to_uppercase();
    assert_eq!(server.api_as(&bearer(&squashed), &USING, calls.clone()).await.0, StatusCode::OK);

    let upload = Request::post(format!("/jmap/upload/{account}/"))
        .header(header::AUTHORIZATION, bearer(&created.secret))
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("purr"))
        .unwrap();
    let (status, body) = server.request(upload).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let blob: Value = serde_json::from_slice(&body).unwrap();
    let download = Request::get(format!("/jmap/download/{account}/{}/purr.txt", blob["blobId"].as_str().unwrap()))
        .header(header::AUTHORIZATION, bearer(&created.secret))
        .body(Body::empty())
        .unwrap();
    let (status, body) = server.request(download).await;
    assert_eq!((status, body.as_slice()), (StatusCode::OK, b"purr".as_slice()));

    // Another person's download stays closed with this token.
    let nyu = server.account_id("nyu@example.org").await;
    let foreign = Request::get(format!("/jmap/download/{nyu}/{}/purr.txt", blob["blobId"].as_str().unwrap()))
        .header(header::AUTHORIZATION, bearer(&created.secret))
        .body(Body::empty())
        .unwrap();
    assert_ne!(server.request(foreign).await.0, StatusCode::OK);

    // Wrong tokens, the account password as a token and a revoked token are refused.
    assert_eq!(server.api_as(&bearer("abcd-efgh-jkmn-pqrs"), &USING, calls.clone()).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(server.api_as(&bearer(PASSWORD), &USING, calls.clone()).await.0, StatusCode::UNAUTHORIZED);
    server.store.revoke_app_password(id, created.app_password.id).await.unwrap();
    assert_eq!(server.api_as(&bearer(&created.secret), &USING, calls.clone()).await.0, StatusCode::UNAUTHORIZED);

    // A password only for SMTP is no JMAP token.
    let smtp_only = server
        .store
        .create_app_password(
            id,
            NewAppPassword { name: "Printer".into(), scopes: vec![AppScope::Smtp], expires_at: None },
        )
        .await
        .unwrap();
    assert_eq!(server.api_as(&bearer(&smtp_only.secret), &USING, calls).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_endpoint_trades_the_password_for_a_named_app_password() {
    let server = server().await;
    let id = server.id("mini@example.org").await;
    let (status, created) = token_request(
        &server,
        json!({ "username": "mini@example.org", "password": PASSWORD, "name": "Mail client on the desk" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["tokenType"], "Bearer");
    assert_eq!(created["accountId"], server.account_id("mini@example.org").await);
    assert_eq!(created["sessionUrl"], "http://mail.example.org/jmap/session");
    assert_eq!(created["expiresAt"], Value::Null);
    let token = created["token"].as_str().unwrap();

    // It is an ordinary app password for mail, and for calendars and contacts, which JMAP offers
    // too; listed with its name.
    let list = server.store.app_passwords(id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "Mail client on the desk");
    assert_eq!(list[0].scopes, vec![AppScope::Dav, AppScope::Mail]);
    let (status, _) = server.api_as(&bearer(token), &USING, json!([["Core/echo", {}, "0"]])).await;
    assert_eq!(status, StatusCode::OK);
    let events = server.store.security_events(id, 10).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "appPasswordCreated"));

    // With an expiry.
    let (status, created) = token_request(
        &server,
        json!({ "username": "mini@example.org", "password": PASSWORD, "name": "Trial", "expiresInDays": 30 }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["expiresAt"].is_string());

    // A wrong password is refused and counts; after too many the network waits.
    for _ in 0..10 {
        let (status, body) =
            token_request(&server, json!({ "username": "mini@example.org", "password": "nope", "name": "x" })).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["type"], "urn:uwumail:jmap:token:invalidCredentials");
    }
    let (status, _) =
        token_request(&server, json!({ "username": "mini@example.org", "password": PASSWORD, "name": "x" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_login_guessed_at_from_many_networks_waits_over_jmap_too() {
    // security-audit-0.16.0 PROTOCOLS-8: JMAP (and DAV, which signs in the same way) and the token
    // endpoint counted per network only, each on its own. Ten wrong passwords for one login from
    // ten networks, over any protocol, and the next try waits here too.
    let server = server().await;
    for network in 0..10 {
        server
            .store
            .auth_limiter()
            .record_failure(format!("2001:db8:{network}::1").parse().unwrap(), "nyu@example.org");
    }
    let (status, _) = server.api_as(&basic("nyu@example.org", PASSWORD), &USING, json!([["Core/echo", {}, "0"]])).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) =
        token_request(&server, json!({ "username": "nyu@example.org", "password": PASSWORD, "name": "x" })).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    // Other logins are not held up.
    let (status, _) =
        server.api_as(&basic("mini@example.org", PASSWORD), &USING, json!([["Core/echo", {}, "0"]])).await;
    assert_eq!(status, StatusCode::OK);

    // And the other way round: what fails over JMAP counts for every other protocol.
    let (status, _) = server.api_as(&basic("ghost@example.org", "nope"), &USING, json!([])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for _ in 0..2 {
        server.api_as(&basic("ghost@example.org", "nope"), &USING, json!([])).await;
    }
    assert!(server.store.auth_limiter().is_blocked("127.0.0.1".parse().unwrap()), "unknown logins count strictly");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_endpoint_asks_for_the_second_factor() {
    let server = server().await;
    let id = server.id("nyu@example.org").await;
    let (_, codes) = server.store.add_passkey(id, vec![1, 2, 3], vec![4, 5, 6], 0, "Key").await.unwrap();
    let recovery = codes.unwrap().remove(0);

    let request = json!({ "username": "nyu@example.org", "password": PASSWORD, "name": "Phone" });
    let (status, body) = token_request(&server, request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["type"], "urn:uwumail:jmap:token:secondFactorRequired");

    let wrong = json!({ "username": "nyu@example.org", "password": PASSWORD, "name": "Phone", "code": "123456" });
    let (status, body) = token_request(&server, wrong).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["type"], "urn:uwumail:jmap:token:invalidCode");

    let right = json!({ "username": "nyu@example.org", "password": PASSWORD, "name": "Phone", "code": recovery });
    let (status, body) = token_request(&server, right).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // The account password itself no longer opens JMAP for mail apps, the token does.
    let calls = json!([["Core/echo", {}, "0"]]);
    assert_eq!(
        server.api_as(&basic("nyu@example.org", PASSWORD), &USING, calls.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(server.api_as(&bearer(body["token"].as_str().unwrap()), &USING, calls).await.0, StatusCode::OK);

    // A request with unknown fields or without a name is refused without counting as a login.
    let (status, _) = token_request(&server, json!({ "username": "nyu@example.org", "password": PASSWORD })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An OAuth access token (docs/oauth.md) is a bearer token too, within the scopes it was given.
#[tokio::test(flavor = "multi_thread")]
async fn oauth_access_tokens_work_as_bearer_tokens() {
    let server = server().await;
    let id = server.id("mini@example.org").await;
    // RFC 7636 appendix B.
    let (verifier, challenge) =
        ("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    let client = server.store.register_oauth_client("Test app", vec!["http://127.0.0.1/cb".into()]).await.unwrap();
    let token = |scopes: Vec<&'static str>| {
        let store = server.store.clone();
        let client = client.clone();
        async move {
            let code = store
                .create_oauth_code(uwumail_store::NewOAuthCode {
                    client_id: client.id,
                    account_id: id,
                    redirect_uri: "http://127.0.0.1/cb".into(),
                    scopes,
                    code_challenge: challenge.into(),
                    nonce: None,
                    auth_time: 0,
                })
                .await
                .unwrap();
            store.redeem_oauth_code(&code, client.id, "http://127.0.0.1/cb", verifier).await.unwrap().unwrap()
        }
    };
    let calls = json!([["Core/echo", { "hello": true }, "0"]]);
    let mail = token(vec!["openid", "mail"]).await;
    let (status, response) = server.api_as(&bearer(&mail.access_token), &USING, calls.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["methodResponses"][0][1]["hello"], true);
    let grants = server.store.oauth_grants(id).await.unwrap();
    assert_eq!(grants[0].last_used_protocol.as_deref(), Some("jmap"));

    // A token for sending only is no way into the mailboxes; neither is the refresh token.
    let sending = token(vec!["smtp"]).await;
    let (status, _) = server.api_as(&bearer(&sending.access_token), &USING, calls.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = server.api_as(&bearer(&mail.refresh_token), &USING, calls.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Signed out in the portal, the token stops working at once.
    server.store.revoke_oauth_grant(id, mail.grant_id).await.unwrap();
    let (status, _) = server.api_as(&bearer(&mail.access_token), &USING, calls).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An app password or app limited to `mail` reads and sends mail; calendars and address books
/// need `dav`, over JMAP as over CalDAV and CardDAV. They used to be open to any JMAP login
/// (security audit 0.16.0 PROTOCOLS-11).
#[tokio::test(flavor = "multi_thread")]
async fn calendars_and_contacts_need_the_dav_scope() {
    let server = server().await;
    let id = server.id("mini@example.org").await;
    let account = server.account_id("mini@example.org").await;
    let app = |scopes: Vec<AppScope>| {
        let store = server.store.clone();
        async move {
            store
                .create_app_password(id, NewAppPassword { name: "app".into(), scopes, expires_at: None })
                .await
                .unwrap()
        }
    };
    let mail_only = app(vec![AppScope::Mail]).await;
    let with_dav = app(vec![AppScope::Mail, AppScope::Dav]).await;
    let using = [
        "urn:ietf:params:jmap:core",
        "urn:ietf:params:jmap:mail",
        "urn:ietf:params:jmap:calendars",
        "urn:ietf:params:jmap:contacts",
    ];
    let session = |secret: String| {
        let server = &server;
        async move {
            let request = Request::get("/jmap/session")
                .header(header::AUTHORIZATION, bearer(&secret))
                .header(header::HOST, "mail.example.org")
                .body(Body::empty())
                .unwrap();
            let (status, body) = server.request(request).await;
            assert_eq!(status, StatusCode::OK);
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    };
    let calls = json!([
        ["Calendar/get", { "accountId": account, "ids": null }, "0"],
        ["AddressBook/get", { "accountId": account, "ids": null }, "1"],
        ["Mailbox/get", { "accountId": account, "ids": null }, "2"]
    ]);

    let limited = session(mail_only.secret.clone()).await;
    assert!(limited["capabilities"].get("urn:ietf:params:jmap:calendars").is_none(), "{limited}");
    assert!(limited["capabilities"].get("urn:ietf:params:jmap:contacts").is_none(), "{limited}");
    assert!(limited["capabilities"].get("urn:ietf:params:jmap:mail").is_some());
    let (status, response) = server.api_as(&bearer(&mail_only.secret), &using, calls.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let responses = response["methodResponses"].as_array().unwrap();
    assert_eq!(responses[0][0], "error", "{}", responses[0]);
    assert_eq!(responses[0][1]["type"], "forbidden");
    assert_eq!(responses[1][1]["type"], "forbidden");
    assert_eq!(responses[2][0], "Mailbox/get", "mail works");

    let full = session(with_dav.secret.clone()).await;
    assert!(full["capabilities"].get("urn:ietf:params:jmap:calendars").is_some(), "{full}");
    let (_, response) = server.api_as(&bearer(&with_dav.secret), &using, calls.clone()).await;
    assert_eq!(response["methodResponses"][0][0], "Calendar/get", "{response}");
    assert_eq!(response["methodResponses"][1][0], "AddressBook/get", "{response}");

    // The account password may do everything.
    let (_, response) = server.api_as(&basic("mini@example.org", PASSWORD), &using, calls).await;
    assert_eq!(response["methodResponses"][0][0], "Calendar/get", "{response}");
}
