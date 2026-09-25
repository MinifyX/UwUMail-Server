//! App passwords as JMAP bearer tokens, and the token endpoint that makes them.

mod common;

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
    assert_eq!(status, StatusCode::CREATED, "{}", String::from_utf8_lossy(&body));
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

    // It is an ordinary app password for mail, listed with its name.
    let list = server.store.app_passwords(id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "Mail client on the desk");
    assert_eq!(list[0].scopes, vec![AppScope::Mail]);
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
