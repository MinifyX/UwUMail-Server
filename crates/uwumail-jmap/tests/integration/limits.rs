//! Limits on what one request may cost the server: bodies are read only after the login, uploads
//! count against a budget, and mail methods have per-request bounds.

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::server;

/// A body that never ends and notes whether anyone started reading it.
fn endless_body() -> (Body, Arc<AtomicBool>) {
    let polled = Arc::new(AtomicBool::new(false));
    let seen = polled.clone();
    let stream = futures_util::stream::poll_fn(move |_| -> Poll<Option<Result<Vec<u8>, std::io::Error>>> {
        seen.store(true, Ordering::SeqCst);
        Poll::Pending
    });
    (Body::from_stream(stream), polled)
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_read_before_the_login() {
    // security-audit-0.16.0 PROTOCOLS-6: uploads (50 MB) and API requests (10 MB) were read whole
    // before the login was checked, so anyone could make the server hold them, and trickle them.
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    for uri in [format!("/jmap/upload/{account}/"), "/jmap/api".to_owned()] {
        let (body, polled) = endless_body();
        let request = Request::post(&uri)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, "40000000")
            .body(body)
            .unwrap();
        let (status, _) = tokio::time::timeout(Duration::from_secs(10), server.request(request))
            .await
            .unwrap_or_else(|_| panic!("{uri}: the server waited for the body"));
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert!(!polled.load(Ordering::SeqCst), "{uri}: the body was read before the login");
    }
}
