//! JMAP over WebSocket (RFC 8887) against a real listener, as a client would connect.

mod common;

use std::time::Duration;

use common::{PASSWORD, USING, basic, server};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn listen(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("ws://{address}/jmap/ws")
}

async fn connect(url: &str, authorization: Option<&str>, protocol: Option<&str>) -> Result<Socket, String> {
    let mut request = url.into_client_request().unwrap();
    if let Some(authorization) = authorization {
        request.headers_mut().insert("authorization", HeaderValue::from_str(authorization).unwrap());
    }
    if let Some(protocol) = protocol {
        request.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_str(protocol).unwrap());
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, response)) => {
            assert_eq!(response.headers()["sec-websocket-protocol"], "jmap");
            Ok(socket)
        }
        Err(err) => Err(err.to_string()),
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket.send(Message::Text(value.to_string().into())).await.unwrap();
}

async fn receive(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next()).await.unwrap().unwrap().unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_and_push_over_a_websocket() {
    let server = server().await;
    let url = listen(server.router.clone()).await;
    let account = server.account_id("mini@example.org").await;
    let authorization = basic("mini@example.org", PASSWORD);

    // Without credentials, or without the jmap subprotocol, there is no connection.
    assert!(connect(&url, None, Some("jmap")).await.is_err());
    assert!(connect(&url, Some(&authorization), None).await.is_err());
    assert!(connect(&url, Some(&basic("mini@example.org", "nope")), Some("jmap")).await.is_err());

    let mut socket = connect(&url, Some(&authorization), Some("jmap")).await.unwrap();

    // A request, answered with its id.
    send(
        &mut socket,
        json!({
            "@type": "Request",
            "id": "r1",
            "using": USING,
            "methodCalls": [["Mailbox/get", { "accountId": account, "properties": ["role"] }, "0"]]
        }),
    )
    .await;
    let response = receive(&mut socket).await;
    assert_eq!(response["@type"], "Response");
    assert_eq!(response["requestId"], "r1");
    assert_eq!(response["methodResponses"][0][0], "Mailbox/get");
    assert!(response["sessionState"].is_string());

    // A broken request is a RequestError with the id.
    send(&mut socket, json!({ "@type": "Request", "id": "r2", "using": ["urn:example:nope"], "methodCalls": [] }))
        .await;
    let error = receive(&mut socket).await;
    assert_eq!(error["@type"], "RequestError");
    assert_eq!(error["requestId"], "r2");
    assert_eq!(error["type"], "urn:ietf:params:jmap:error:unknownCapability");

    // Push: nothing before WebSocketPushEnable, then StateChange with a pushState.
    send(&mut socket, json!({ "@type": "WebSocketPushEnable", "dataTypes": ["Email", "Mailbox"] })).await;
    // Messages are handled in order: once this is answered, push is on.
    send(&mut socket, json!({ "@type": "Request", "id": "sync", "using": USING, "methodCalls": [] })).await;
    assert_eq!(receive(&mut socket).await["requestId"], "sync");
    server.deliver("mini@example.org", "From: nyu@example.org\nTo: mini@example.org\nSubject: Hi\n\nPurr\n").await;
    let change = receive(&mut socket).await;
    assert_eq!(change["@type"], "StateChange");
    assert!(change["changed"][&account]["Email"].is_string());
    assert!(change["changed"][&account].get("Thread").is_none(), "only the types asked for");
    let push_state = change["pushState"].as_str().unwrap().to_owned();

    // Another person's mail is not pushed here.
    server.deliver("nyu@example.org", "From: mini@example.org\nTo: nyu@example.org\nSubject: Hi\n\nPurr\n").await;

    // Switched off, nothing comes; a request still works and is the next message.
    send(&mut socket, json!({ "@type": "WebSocketPushDisable" })).await;
    send(
        &mut socket,
        json!({ "@type": "Request", "id": "r3", "using": USING, "methodCalls": [["Core/echo", {}, "e"]] }),
    )
    .await;
    assert_eq!(receive(&mut socket).await["requestId"], "r3");
    server.deliver("mini@example.org", "From: nyu@example.org\nTo: mini@example.org\nSubject: Two\n\nPurr\n").await;
    send(
        &mut socket,
        json!({ "@type": "Request", "id": "r4", "using": USING, "methodCalls": [["Core/echo", {}, "e"]] }),
    )
    .await;
    assert_eq!(receive(&mut socket).await["requestId"], "r4", "no push while it is disabled");

    // Enabling again with the last pushState catches up at once.
    send(&mut socket, json!({ "@type": "WebSocketPushEnable", "dataTypes": null, "pushState": push_state })).await;
    let change = receive(&mut socket).await;
    assert_eq!(change["@type"], "StateChange");
    assert!(change["changed"][&account]["Email"].is_string());
    assert_ne!(change["pushState"], push_state);
}
