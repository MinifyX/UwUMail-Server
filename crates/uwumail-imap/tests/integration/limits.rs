//! Connections per client address: one address may not hold every connection slot of IMAP or
//! ManageSieve, while other addresses and trusted relays go on as before.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use uwumail_imap::{Imap, ManageSieve};
use uwumail_store::Store;

struct Setup {
    store: Store,
    tls: Arc<rustls::ServerConfig>,
    connector: tokio_rustls::TlsConnector,
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.cert.der().clone()], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Setup { store, tls: Arc::new(tls), connector: tokio_rustls::TlsConnector::from(Arc::new(client)), _dir: dir }
}

/// An IMAP connection on port 993 from `peer`: `Some(stream)` once greeted, `None` when the server
/// closed it.
async fn imap_from(
    setup: &Setup,
    imap: &Imap,
    peer: &str,
) -> Option<BufReader<tokio_rustls::client::TlsStream<tokio::io::DuplexStream>>> {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let peer: SocketAddr = peer.parse().unwrap();
    imap.serve_stream(Box::new(server), peer, setup.tls.clone());
    let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = setup.connector.connect(name, client).await.ok()?;
    let mut stream = BufReader::new(tls);
    let mut greeting = String::new();
    stream.read_line(&mut greeting).await.ok()?;
    assert!(greeting.starts_with("* OK"), "{greeting}");
    Some(stream)
}

#[tokio::test]
async fn one_address_cannot_take_every_imap_connection() {
    let setup = setup().await;
    let imap = Imap::new(setup.store.clone(), 1024 * 1024).with_client_limit(2);
    let first = imap_from(&setup, &imap, "192.0.2.7:40000").await.expect("the first connection is served");
    let _second = imap_from(&setup, &imap, "192.0.2.7:40001").await.expect("the second as well");
    assert!(imap_from(&setup, &imap, "192.0.2.7:40002").await.is_none(), "a third from the same address is closed");
    // IPv6 clients count per /64.
    let _v6 = imap_from(&setup, &imap, "[2001:db8:1:2::1]:40000").await.unwrap();
    let _v6b = imap_from(&setup, &imap, "[2001:db8:1:2::2]:40000").await.unwrap();
    assert!(imap_from(&setup, &imap, "[2001:db8:1:2:ffff::3]:40000").await.is_none(), "the same /64");
    // Everyone else is not affected.
    assert!(imap_from(&setup, &imap, "198.51.100.9:40000").await.is_some(), "another address is served");

    // A closed connection frees its place, once the server noticed.
    drop(first);
    let mut served = false;
    for _ in 0..200 {
        if imap_from(&setup, &imap, "192.0.2.7:40003").await.is_some() {
            served = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(served, "the address got its place back");
}

#[tokio::test]
async fn a_trusted_relay_is_not_limited_like_one_client() {
    let setup = setup().await;
    let imap = Imap::new(setup.store.clone(), 1024 * 1024)
        .with_client_limit(1)
        .trusting(|ip| ip == "203.0.113.25".parse::<std::net::IpAddr>().unwrap());
    let mut open = Vec::new();
    for port in 0..3 {
        let peer = format!("203.0.113.25:{}", 40000 + port);
        open.push(imap_from(&setup, &imap, &peer).await.expect("the relay is served"));
    }
    let _one = imap_from(&setup, &imap, "192.0.2.8:40000").await.unwrap();
    assert!(imap_from(&setup, &imap, "192.0.2.8:40001").await.is_none(), "others are still limited");
}

/// A ManageSieve connection from `peer`: its first answer (the greeting, or the BYE of a refused
/// connection) and the stream to keep it open.
async fn sieve_from(setup: &Setup, sieve: &ManageSieve, peer: &str) -> (String, BufReader<tokio::io::DuplexStream>) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    sieve.serve_stream(Box::new(server), peer.parse().unwrap(), setup.tls.clone());
    let mut stream = BufReader::new(client);
    let mut text = String::new();
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read_line(&mut line))
            .await
            .expect("the server answered in time")
            .unwrap();
        text.push_str(&line);
        if read == 0 || line.starts_with("OK") || line.starts_with("BYE") {
            return (text, stream);
        }
    }
}

#[tokio::test]
async fn one_address_cannot_take_every_managesieve_connection() {
    let setup = setup().await;
    let imap = Imap::new(setup.store.clone(), 1024 * 1024);
    let sieve = ManageSieve::new(&imap).with_client_limit(2);
    let mut open = Vec::new();
    for port in 0..2 {
        let (greeting, stream) = sieve_from(&setup, &sieve, &format!("192.0.2.7:{}", 40000 + port)).await;
        assert!(greeting.contains("\"IMPLEMENTATION\"") && greeting.contains("\nOK"), "{greeting}");
        open.push(stream);
    }
    let (refused, mut stream) = sieve_from(&setup, &sieve, "192.0.2.7:40002").await;
    assert!(refused.starts_with("BYE"), "{refused}");
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty(), "the refused connection is closed: {}", String::from_utf8_lossy(&rest));

    let (other, _stream) = sieve_from(&setup, &sieve, "198.51.100.9:40000").await;
    assert!(other.contains("\"IMPLEMENTATION\""), "another address is served: {other}");
}
