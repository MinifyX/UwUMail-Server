//! TLS client settings for outgoing connections.

use std::sync::{Arc, Mutex};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

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
        let verifier = Arc::new(ReportOnly { inner: self.webpki.clone(), seen: Mutex::new(None) });
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

/// Accepts every certificate, and remembers what the web PKI check had against it.
#[derive(Debug)]
pub(crate) struct ReportOnly {
    inner: Arc<WebPkiServerVerifier>,
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
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
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
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
