//! TLS client settings for outgoing connections.

use std::sync::{Arc, Mutex};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    verify_tls13_signature_with_raw_key,
};
use rustls::server::ParsedCertificate;
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};

use crate::SmtpError;
use crate::dane::{DaneMismatch, DaneVerifier, Tlsa};
use crate::tlsrpt::ResultType;

pub struct ClientTls {
    /// For MX delivery: encrypt whenever possible, like every other mail server does.
    /// Certificates of MX hosts are routinely self-signed or issued for another name.
    pub opportunistic: Arc<ClientConfig>,
    /// For relays: the certificate must be valid for the host name.
    pub verified: Arc<ClientConfig>,
    provider: Arc<CryptoProvider>,
    /// The check `verified` makes, for connections that only report what it finds.
    webpki: Arc<WebPkiServerVerifier>,
}

impl ClientTls {
    pub fn new() -> Result<ClientTls, SmtpError> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

        let opportunistic = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider.clone())))
            .with_no_client_auth();

        let roots = Arc::new(RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
        let webpki = WebPkiServerVerifier::builder_with_provider(roots.clone(), provider.clone())
            .build()
            .map_err(|err| SmtpError::Config(err.to_string()))?;
        let verified = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();

        Ok(ClientTls { opportunistic: Arc::new(opportunistic), verified: Arc::new(verified), provider, webpki })
    }

    fn custom(&self, verifier: Arc<dyn ServerCertVerifier>) -> Result<Arc<ClientConfig>, String> {
        let config = ClientConfig::builder_with_provider(self.provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|err| err.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        Ok(Arc::new(config))
    }

    /// For an MX host with usable TLSA records: its certificate must match one of them (RFC 7672).
    /// `names` are the MX host and the recipient's domain, which DANE-TA certificates must name.
    pub(crate) fn dane(&self, records: Arc<[Tlsa]>, names: &[&str]) -> Result<Arc<ClientConfig>, String> {
        let names =
            names.iter().filter_map(|name| ServerName::try_from(name.trim_end_matches('.').to_owned()).ok()).collect();
        self.custom(Arc::new(DaneVerifier { records, names, provider: self.provider.clone() }))
    }

    /// Checks the certificate like `verified`, but only notes what is wrong with it: for a domain
    /// whose MTA-STS policy is in testing mode, which wants failures reported, not enforced.
    pub(crate) fn report_only(&self) -> Result<(Arc<ClientConfig>, Arc<ReportOnly>), String> {
        let verifier = Arc::new(ReportOnly {
            inner: self.webpki.clone(),
            provider: self.provider.clone(),
            seen: Mutex::new(None),
        });
        Ok((self.custom(verifier.clone())?, verifier))
    }
}

/// What went wrong in a TLS handshake, in the words of a TLS report (RFC 8460, section 4.3).
pub(crate) fn result_type(err: &std::io::Error) -> ResultType {
    match err.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
        Some(err) => certificate_result(err),
        None => ResultType::ValidationFailure,
    }
}

pub(crate) fn certificate_result(err: &rustls::Error) -> ResultType {
    let rustls::Error::InvalidCertificate(problem) = err else {
        return ResultType::ValidationFailure;
    };
    match problem {
        CertificateError::Expired
        | CertificateError::ExpiredContext { .. }
        | CertificateError::NotValidYet
        | CertificateError::NotValidYetContext { .. } => ResultType::CertificateExpired,
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            ResultType::CertificateHostMismatch
        }
        CertificateError::UnknownIssuer | CertificateError::BadSignature | CertificateError::BadEncoding => {
            ResultType::CertificateNotTrusted
        }
        CertificateError::Other(other) if other.0.downcast_ref::<DaneMismatch>().is_some() => ResultType::TlsaInvalid,
        _ => ResultType::ValidationFailure,
    }
}

/// The certificate's SubjectPublicKeyInfo, also for X.509 v1 certificates, which webpki refuses.
pub(crate) fn public_key_info(cert: &CertificateDer<'_>) -> Option<Vec<u8>> {
    if let Ok(parsed) = ParsedCertificate::try_from(cert) {
        return Some(parsed.subject_public_key_info().as_ref().to_vec());
    }
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).ok()?;
    Some(parsed.public_key().raw.to_vec())
}

/// Checks the server's handshake signature against the key in its certificate.
///
/// webpki refuses X.509 v1 certificates, and self-signed MX certificates often are v1 (Proxmox
/// Mail Gateway makes them so): for those the key is taken from the certificate directly, or the
/// handshake would fail although no certificate check asked for more than the key.
pub(crate) fn verify_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
    algorithms: &WebPkiSupportedAlgorithms,
    tls13: bool,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    let checked = match tls13 {
        true => verify_tls13_signature(message, cert, dss, algorithms),
        false => verify_tls12_signature(message, cert, dss, algorithms),
    };
    let Err(err) = checked else { return checked };
    if ParsedCertificate::try_from(cert).is_ok() {
        return Err(err);
    }
    let Some(spki) = public_key_info(cert) else { return Err(err) };
    if tls13 {
        return verify_tls13_signature_with_raw_key(message, &SubjectPublicKeyInfoDer::from(spki), dss, algorithms);
    }
    // TLS 1.2 schemes name several algorithms; the one for the certificate's key type decides.
    let (key_algorithm, key) = split_public_key_info(&spki).ok_or(err)?;
    let bad = || rustls::Error::InvalidCertificate(CertificateError::BadSignature);
    let alg = algorithms
        .mapping
        .iter()
        .filter(|(scheme, _)| *scheme == dss.scheme)
        .flat_map(|(_, candidates)| candidates.iter())
        .find(|alg| alg.public_key_alg_id().as_ref() == key_algorithm)
        .ok_or_else(bad)?;
    alg.verify_signature(key, message, dss.signature()).map_err(|_| bad())?;
    Ok(HandshakeSignatureValid::assertion())
}

/// The algorithm identifier's contents and the key bits of a DER SubjectPublicKeyInfo.
fn split_public_key_info(spki: &[u8]) -> Option<(&[u8], &[u8])> {
    let (body, _) = der_element(spki, 0x30)?;
    let (algorithm, rest) = der_element(body, 0x30)?;
    let (bits, _) = der_element(rest, 0x03)?;
    // The first byte of a BIT STRING counts its unused bits, none for a key.
    match bits.split_first()? {
        (0, key) => Some((algorithm, key)),
        _ => None,
    }
}

/// The contents of the DER element of type `tag` at the start of `input`, and what follows it.
fn der_element(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&found, rest) = input.split_first()?;
    if found != tag {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = match first {
        0..=0x7f => (usize::from(first), rest),
        0x81..=0x84 => {
            let count = usize::from(first & 0x7f);
            if rest.len() < count {
                return None;
            }
            let (bytes, rest) = rest.split_at(count);
            (bytes.iter().fold(0usize, |len, &b| (len << 8) | usize::from(b)), rest)
        }
        _ => return None,
    };
    (rest.len() >= len).then(|| rest.split_at(len))
}

/// Accepts every certificate, and remembers what the web PKI check had against it.
#[derive(Debug)]
pub(crate) struct ReportOnly {
    inner: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    seen: Mutex<Option<rustls::Error>>,
}

impl ReportOnly {
    /// What was wrong with the certificate, if anything.
    pub(crate) fn problem(&self) -> Option<rustls::Error> {
        self.seen.lock().expect("verifier poisoned").clone()
    }
}

impl ServerCertVerifier for ReportOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Err(err) = self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            *self.seen.lock().expect("verifier poisoned") = Some(err);
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.provider.signature_verification_algorithms, false)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.provider.signature_verification_algorithms, true)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.0.signature_verification_algorithms, false)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.0.signature_verification_algorithms, true)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rcgen::{KeyPair, PublicKeyData, SignatureAlgorithm};
    use rustls::server::{ClientHello, ResolvesServerCert};
    use rustls::sign::CertifiedKey;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{ClientTls, public_key_info};

    fn der(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        match body.len() {
            len @ 0..=0x7f => out.push(len as u8),
            len @ 0x80..=0xff => out.extend([0x81, len as u8]),
            len => out.extend([0x82, (len >> 8) as u8, len as u8]),
        }
        out.extend_from_slice(body);
        out
    }

    /// A self-signed X.509 v1 certificate, as Proxmox Mail Gateway makes them: no version field,
    /// no extensions. Its own signature is junk, which no opportunistic check looks at.
    fn v1_certificate(key: &KeyPair) -> CertificateDer<'static> {
        let algorithm = der(0x30, &[der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02])].concat());
        let name = der(0x30, &[]);
        let validity = der(0x30, &[der(0x17, b"250101000000Z"), der(0x17, b"491231235959Z")].concat());
        let tbs = der(
            0x30,
            &[der(0x02, &[1]), algorithm.clone(), name.clone(), validity, name, key.subject_public_key_info()].concat(),
        );
        CertificateDer::from(der(0x30, &[tbs, algorithm, der(0x03, &[0, 1, 2, 3])].concat()))
    }

    #[derive(Debug)]
    struct Fixed(Arc<CertifiedKey>);

    impl ResolvesServerCert for Fixed {
        fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    async fn handshake(alg: &'static SignatureAlgorithm, version: &'static rustls::SupportedProtocolVersion) {
        let key = KeyPair::generate_for(alg).unwrap();
        let cert = v1_certificate(&key);
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let signer = provider
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())))
            .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Fixed(Arc::new(CertifiedKey::new(vec![cert.clone()], signer)))));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));

        let (near, far) = tokio::io::duplex(64 * 1024);
        let serve = tokio::spawn(async move {
            let mut stream = acceptor.accept(far).await.unwrap();
            stream.write_all(b"220 ok\r\n").await.unwrap();
            stream.flush().await.unwrap();
        });
        let tls = ClientTls::new().unwrap();
        let connector = tokio_rustls::TlsConnector::from(tls.opportunistic.clone());
        let name = ServerName::try_from("mx.example.com").unwrap();
        let mut stream = connector.connect(name, near).await.expect("handshake with a v1 certificate");
        let mut line = [0; 8];
        stream.read_exact(&mut line).await.unwrap();
        assert_eq!(&line, b"220 ok\r\n");
        serve.await.unwrap();

        assert_eq!(public_key_info(&cert), Some(key.subject_public_key_info()));
    }

    #[tokio::test]
    async fn opportunistic_tls_accepts_x509_v1_certificates() {
        handshake(&rcgen::PKCS_ECDSA_P256_SHA256, &rustls::version::TLS12).await;
        handshake(&rcgen::PKCS_ECDSA_P256_SHA256, &rustls::version::TLS13).await;
        handshake(&rcgen::PKCS_RSA_SHA256, &rustls::version::TLS12).await;
    }
}
