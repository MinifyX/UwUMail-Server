//! `POST /jmap/image/{accountId}/sizes`: the sizes of a message's remote pictures, one line of JSON
//! each (docs/jmap-remote.md). The fetching itself is tested in uwumail-smtp's `remote_images`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};

use crate::common::{PASSWORD, basic, server};

fn sizes(account: &str, login: &str, body: String) -> Request<Body> {
    Request::post(format!("/jmap/image/{account}/sizes"))
        .header(header::AUTHORIZATION, basic(login, PASSWORD))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn sizes_are_told_line_by_line_and_only_for_the_web() {
    let server = server().await;
    let session = server.session_of("mini@example.org").await;
    let announced = session["capabilities"]["urn:uwumail:jmap:remote"]["imageSizesUrl"].as_str().unwrap();
    assert!(announced.ends_with("/jmap/image/{accountId}/sizes"), "{announced}");
    let account = server.account_id("mini@example.org").await;
    let urls = json!({ "urls": ["http://127.0.0.1/admin.png", "file:///etc/passwd", "http://127.0.0.1/admin.png"] });
    let (status, body) = server.request(sizes(&account, "mini@example.org", urls.to_string())).await;
    assert_eq!(status, StatusCode::OK);
    let mut lines: Vec<Value> =
        String::from_utf8(body).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    lines.sort_by_key(|line| line["url"].as_str().unwrap_or_default().to_owned());
    assert_eq!(
        lines,
        [
            json!({ "url": "file:///etc/passwd", "failed": true }),
            json!({ "url": "http://127.0.0.1/admin.png", "failed": true }),
        ],
        "each address once, and nothing from inside"
    );

    let many = json!({ "urls": (0..201).map(|n| format!("https://pictures.example/{n}.png")).collect::<Vec<_>>() });
    assert_eq!(server.request(sizes(&account, "mini@example.org", many.to_string())).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(server.request(sizes(&account, "mini@example.org", "[]".into())).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        server.request(sizes(&account, "nyu@example.org", urls.to_string())).await.0,
        StatusCode::NOT_FOUND,
        "not someone else's account"
    );
    let anonymous = Request::post(format!("/jmap/image/{account}/sizes")).body(Body::from(urls.to_string())).unwrap();
    assert_eq!(server.request(anonymous).await.0, StatusCode::UNAUTHORIZED);
}
