//! What other domains publish on the web (MTA-STS policies, and what the DNS check reads) is
//! fetched from public addresses only: the names come from other people's DNS, which may point them
//! at this host or the local network.

use std::time::Duration;

use tokio::net::TcpListener;
use uwumail_smtp::https::Https;

#[tokio::test]
async fn a_policy_host_on_a_private_address_is_not_asked() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let https = Https::new();
    for url in [
        format!("https://localhost:{port}/.well-known/mta-sts.txt"),
        format!("https://127.0.0.1:{port}/.well-known/mta-sts.txt"),
        format!("https://[::ffff:127.0.0.1]:{port}/.well-known/mta-sts.txt"),
    ] {
        let error = https.get(&url, 64 * 1024, Duration::from_secs(10)).await.unwrap_err();
        assert!(error.contains("not a public address"), "{url}: {error}");
    }
    // Nothing even connected: a connection would be waiting to be accepted by now.
    let accepted = tokio::time::timeout(Duration::ZERO, listener.accept()).await;
    assert!(accepted.is_err(), "the fetch reached the private address");
}
