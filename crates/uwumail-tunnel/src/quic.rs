//! QUIC endpoints with the tunnel's TLS settings.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig, TransportConfig, VarInt};
use rustls::crypto::CryptoProvider;
use rustls_pki_types::CertificateDer;

use crate::TunnelError;
use crate::identity::{Fingerprint, Identity};
use crate::verify::{AnyClient, PinnedServer};

const ALPN: &[u8] = b"uwumail-tunnel/1";
/// Keeps the mapping of home routers and carrier-grade NAT open while nothing else is sent.
const KEEP_ALIVE: Duration = Duration::from_secs(10);
/// A connection without any sign of life for this long counts as lost.
const IDLE_TIMEOUT_MS: u32 = 30_000;
/// Connections carried at once in each direction.
const MAX_STREAMS: u32 = 4096;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn transport() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .keep_alive_interval(Some(KEEP_ALIVE))
        .max_idle_timeout(Some(VarInt::from_u32(IDLE_TIMEOUT_MS).into()));
    Arc::new(transport)
}

/// The gateway's endpoint: waits for the tunnel from a server on `address` (UDP).
pub fn server_endpoint(address: SocketAddr, identity: &Identity) -> Result<Endpoint, TunnelError> {
    let provider = provider();
    let verifier = Arc::new(AnyClient { algorithms: provider.signature_verification_algorithms });
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![identity.certificate()], identity.private_key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicServerConfig::try_from(tls).map_err(|err| TunnelError::Quic(err.to_string()))?;
    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport());
    Ok(Endpoint::server(config, address)?)
}

/// The server's side: shows `identity` and trusts only the gateway certificate with `gateway`'s fingerprint.
pub fn client_config(identity: &Identity, gateway: Fingerprint) -> Result<ClientConfig, TunnelError> {
    let provider = provider();
    let verifier = Arc::new(PinnedServer { pin: gateway, algorithms: provider.signature_verification_algorithms });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![identity.certificate()], identity.private_key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(tls).map_err(|err| TunnelError::Quic(err.to_string()))?;
    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport());
    Ok(config)
}

/// The fingerprint of the certificate the other side showed in the handshake.
pub fn peer_fingerprint(connection: &Connection) -> Option<Fingerprint> {
    let identity = connection.peer_identity()?;
    let certificates = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certificates.first().map(|certificate| Fingerprint::of(certificate))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::identity::CERTIFICATE_NAME;

    async fn handshake(gateway: &Identity, pin: Fingerprint, server: &Identity) -> Result<Option<Fingerprint>, String> {
        let endpoint = server_endpoint((Ipv4Addr::LOCALHOST, 0).into(), gateway).unwrap();
        let address = endpoint.local_addr().unwrap();
        let accepting = tokio::spawn(async move {
            let incoming = endpoint.accept().await?;
            let connection = incoming.await.ok()?;
            let seen = peer_fingerprint(&connection);
            // Keep the endpoint alive until the client is done.
            connection.closed().await;
            seen
        });

        let client = Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let config = client_config(server, pin).unwrap();
        let connection =
            client.connect_with(config, address, CERTIFICATE_NAME).unwrap().await.map_err(|err| err.to_string())?;
        connection.close(VarInt::from_u32(0), b"done");
        Ok(accepting.await.unwrap())
    }

    #[tokio::test]
    async fn only_the_pinned_gateway_is_accepted() {
        let gateway = Identity::generate().unwrap();
        let server = Identity::generate().unwrap();

        let seen = handshake(&gateway, gateway.fingerprint(), &server).await.unwrap();
        assert_eq!(seen, Some(server.fingerprint()), "the gateway learns who is calling");

        let impostor = Identity::generate().unwrap();
        let error = handshake(&impostor, gateway.fingerprint(), &server).await.unwrap_err();
        assert!(error.contains("certificate") || error.contains("handshake"), "{error}");
    }
}
