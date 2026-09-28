//! DANE for outgoing mail (RFC 7672): TLSA records, looked up with DNSSEC validation, that say
//! which certificate a domain's MX host presents.
//!
//! The MX lookup and the TLSA answer both have to validate as secure; only then do the records
//! count. A domain whose MX host has usable records (DANE-TA and DANE-EE) gets mail only over
//! STARTTLS with a certificate that matches one of them, and DANE comes before MTA-STS. Answers
//! that fail validation (bogus) hold the mail back. When the validating lookups themselves do not
//! work, e.g. because this network's resolver drops DNSSEC signatures, mail goes out as it did
//! without DANE and the log says so.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_lc_rs::digest;
use hickory_resolver::TokioResolver;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::dnssec::Proof;
use hickory_resolver::proto::rr::{RData, RecordType};
use mail_auth::ResolverCache;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::server::ParsedCertificate;
use rustls::{CertificateError, DigitallySignedStruct, OtherError, RootCertStore, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use crate::Context;

/// How long one validating lookup may take before mail goes on without DANE.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long answers are kept: within these bounds of their TTL.
const MIN_TTL: u64 = 60;
const MAX_TTL: u64 = 3600;
/// After a lookup that did not work at all, it is not asked again for this long.
const FAILED_TTL: u64 = 300;
/// How long the resolver's DNSSEC support is trusted, or distrusted, before asking again.
const PROBE_OK: Duration = Duration::from_secs(6 * 3600);
const PROBE_FAILED: Duration = Duration::from_secs(900);

/// One TLSA record (RFC 6698).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tlsa {
    /// 0 PKIX-TA, 1 PKIX-EE, 2 DANE-TA, 3 DANE-EE.
    pub usage: u8,
    /// 0 the whole certificate, 1 its public key (SubjectPublicKeyInfo).
    pub selector: u8,
    /// 0 the data itself, 1 SHA-256, 2 SHA-512.
    pub matching: u8,
    pub data: Vec<u8>,
}

impl Tlsa {
    /// Whether a mail server may use it: RFC 7672 leaves out the PKIX usages, and unknown
    /// selectors and matching types say nothing.
    pub fn usable(&self) -> bool {
        matches!(self.usage, 2 | 3) && self.selector <= 1 && self.matching <= 2
    }

    /// Whether `cert` is the certificate (or carries the key) this record names.
    pub fn matches(&self, cert: &CertificateDer<'_>) -> bool {
        let selected = match self.selector {
            0 => cert.as_ref().to_vec(),
            1 => match ParsedCertificate::try_from(cert) {
                Ok(parsed) => parsed.subject_public_key_info().as_ref().to_vec(),
                Err(_) => return false,
            },
            _ => return false,
        };
        match self.matching {
            0 => selected == self.data,
            1 => digest::digest(&digest::SHA256, &selected).as_ref() == self.data.as_slice(),
            2 => digest::digest(&digest::SHA512, &selected).as_ref() == self.data.as_slice(),
            _ => false,
        }
    }

    /// `3 1 1 0a1b…`, as written in a zone file.
    pub fn parse(text: &str) -> Option<Tlsa> {
        let mut parts = text.split_whitespace();
        let usage = parts.next()?.parse().ok()?;
        let selector = parts.next()?.parse().ok()?;
        let matching = parts.next()?.parse().ok()?;
        let data = hex::decode(parts.collect::<String>()).ok()?;
        (!data.is_empty()).then_some(Tlsa { usage, selector, matching, data })
    }
}

impl fmt::Display for Tlsa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} {}", self.usage, self.selector, self.matching, hex::encode(&self.data))
    }
}

/// The `3 1 1` record for a certificate: its public key's SHA-256. It stays valid across renewals
/// that keep the key.
pub fn dane_ee_record(cert: &CertificateDer<'_>) -> Option<Tlsa> {
    let parsed = ParsedCertificate::try_from(cert).ok()?;
    let spki = parsed.subject_public_key_info();
    Some(Tlsa {
        usage: 3,
        selector: 1,
        matching: 1,
        data: digest::digest(&digest::SHA256, spki.as_ref()).as_ref().to_vec(),
    })
}

/// The name TLSA records for SMTP on `host` are published at.
pub fn tlsa_name(host: &str) -> String {
    format!("_25._tcp.{}", host.trim_end_matches('.').to_ascii_lowercase())
}

/// What DNSSEC says about an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Secure,
    /// Unsigned, or the lookup did not work: DANE does not apply.
    Insecure,
    /// Signed, but the signatures do not validate.
    Bogus,
}

/// The TLSA records of one MX host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostTlsa {
    /// None, or none that validate as secure.
    None,
    /// Validated records, usable or not.
    Records(Arc<[Tlsa]>),
    Bogus,
}

/// How DANE applies to one MX host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Dane {
    Off,
    /// STARTTLS with a certificate one of these usable records matches.
    Verify(Arc<[Tlsa]>),
    /// Records exist, but none is usable: STARTTLS without checking the certificate (RFC 7672,
    /// section 2.2).
    EncryptOnly(Arc<[Tlsa]>),
    /// The TLSA answer does not validate: this host gets no mail for now.
    Bogus,
}

impl Dane {
    pub(crate) fn of(host: HostTlsa) -> Dane {
        match host {
            HostTlsa::None => Dane::Off,
            HostTlsa::Bogus => Dane::Bogus,
            HostTlsa::Records(records) => {
                let usable: Arc<[Tlsa]> = records.iter().filter(|record| record.usable()).cloned().collect();
                if usable.is_empty() { Dane::EncryptOnly(records) } else { Dane::Verify(usable) }
            }
        }
    }

    pub(crate) fn applies(&self) -> bool {
        !matches!(self, Dane::Off)
    }

    /// The records as a TLS report names the policy.
    pub(crate) fn policy_strings(&self) -> Vec<String> {
        match self {
            Dane::Verify(records) | Dane::EncryptOnly(records) => records.iter().map(ToString::to_string).collect(),
            Dane::Off | Dane::Bogus => Vec::new(),
        }
    }
}

/// The error a certificate gets that no TLSA record matches.
#[derive(Debug)]
pub struct DaneMismatch;

impl fmt::Display for DaneMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no TLSA record matches the certificate")
    }
}

impl std::error::Error for DaneMismatch {}

fn mismatch() -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(DaneMismatch))))
}

/// Checks the server's certificate against TLSA records (RFC 7672, section 3).
///
/// DANE-EE (3) matches the server's own certificate or key, whatever names and dates it carries.
/// DANE-TA (2) matches a certificate the server sends along in its chain; the chain from it down
/// to the server's certificate must hold, and that certificate must be valid for one of `names`
/// (the MX host, then the recipient's domain).
pub(crate) fn verify(
    records: &[Tlsa],
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    names: &[ServerName<'static>],
    now: UnixTime,
    provider: &Arc<CryptoProvider>,
) -> Result<ServerCertVerified, rustls::Error> {
    if records.iter().filter(|record| record.usage == 3 && record.usable()).any(|record| record.matches(end_entity)) {
        return Ok(ServerCertVerified::assertion());
    }
    let mut last = None;
    for record in records.iter().filter(|record| record.usage == 2 && record.usable()) {
        for anchor in intermediates.iter().filter(|cert| record.matches(cert)) {
            let mut roots = RootCertStore::empty();
            if roots.add(anchor.clone().into_owned()).is_err() {
                continue;
            }
            let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .map_err(|err| rustls::Error::General(err.to_string()))?;
            for name in names {
                match verifier.verify_server_cert(end_entity, intermediates, name, &[], now) {
                    Ok(verified) => return Ok(verified),
                    Err(err) => last = Some(err),
                }
            }
        }
    }
    Err(last.unwrap_or_else(mismatch))
}

/// The certificate check of a connection to an MX host with usable TLSA records.
#[derive(Debug)]
pub(crate) struct DaneVerifier {
    pub records: Arc<[Tlsa]>,
    pub names: Vec<ServerName<'static>>,
    pub provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for DaneVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        verify(&self.records, end_entity, intermediates, &self.names, now, &self.provider)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A validated answer.
#[derive(Debug)]
enum Checked {
    Secure(Vec<RData>, u64),
    /// No such record, or not signed.
    Insecure,
    Bogus,
    /// The lookup or its validation did not work.
    Failed(String),
}

/// The DNSSEC-validating resolver: the system's resolvers for the questions, the root's trust
/// anchor for the answers, so it works even when those resolvers do not validate themselves.
#[derive(Default)]
pub struct Validator {
    resolver: tokio::sync::OnceCell<Option<TokioResolver>>,
    /// Whether signed answers make it through here, and until when that is believed.
    probe: Mutex<Option<(bool, Instant)>>,
}

impl Validator {
    async fn resolver(&self) -> Option<&TokioResolver> {
        self.resolver
            .get_or_init(|| async {
                let built = TokioResolver::builder_tokio().and_then(|mut builder| {
                    let options = builder.options_mut();
                    options.validate = true;
                    options.try_tcp_on_error = true;
                    options.timeout = Duration::from_secs(4);
                    builder.build()
                });
                match built {
                    Ok(resolver) => Some(resolver),
                    Err(err) => {
                        tracing::warn!(%err, "no DNSSEC-validating resolver, outgoing mail goes without DANE");
                        None
                    }
                }
            })
            .await
            .as_ref()
    }

    /// Whether the root zone's signed answer validates: without that, every signed zone would
    /// look bogus, e.g. behind a resolver that strips the signatures.
    async fn works(&self) -> bool {
        if let Some((works, until)) = *self.probe.lock().expect("probe poisoned")
            && until > Instant::now()
        {
            return works;
        }
        let works = matches!(self.lookup(".", RecordType::SOA).await, Checked::Secure(..));
        if !works {
            tracing::warn!(
                "DNSSEC answers do not validate through this server's resolver, outgoing mail goes without DANE"
            );
        }
        let until = Instant::now() + if works { PROBE_OK } else { PROBE_FAILED };
        *self.probe.lock().expect("probe poisoned") = Some((works, until));
        works
    }

    async fn lookup(&self, name: &str, record_type: RecordType) -> Checked {
        let Some(resolver) = self.resolver().await else {
            return Checked::Failed("no resolver".into());
        };
        let name = format!("{}.", name.trim_end_matches('.'));
        let answer = match tokio::time::timeout(LOOKUP_TIMEOUT, resolver.lookup(name.as_str(), record_type)).await {
            Ok(answer) => answer,
            Err(_) => return Checked::Failed("the lookup timed out".into()),
        };
        match answer {
            Ok(lookup) => {
                let records: Vec<_> =
                    lookup.answers().iter().filter(|record| record.record_type() != RecordType::RRSIG).collect();
                if records.iter().any(|record| record.proof == Proof::Bogus) {
                    Checked::Bogus
                } else if records.is_empty() {
                    Checked::Insecure
                } else if records.iter().all(|record| record.proof == Proof::Secure) {
                    let ttl = records.iter().map(|record| u64::from(record.ttl)).min().unwrap_or(MIN_TTL);
                    let data = records
                        .iter()
                        .filter(|record| record.record_type() == record_type)
                        .map(|record| record.data.clone())
                        .collect();
                    Checked::Secure(data, ttl.clamp(MIN_TTL, MAX_TTL))
                } else if records.iter().any(|record| record.proof == Proof::Indeterminate) {
                    Checked::Failed("the answer could not be validated".into())
                } else {
                    Checked::Insecure
                }
            }
            Err(NetError::Dns(DnsError::NoRecordsFound(_))) => Checked::Insecure,
            Err(NetError::Dns(DnsError::Nsec { proof: Proof::Bogus, .. })) => Checked::Bogus,
            Err(NetError::Dns(DnsError::Nsec { .. })) => Checked::Insecure,
            Err(NetError::Dns(DnsError::DnssecBogus)) => Checked::Bogus,
            Err(err) => Checked::Failed(err.to_string()),
        }
    }

    /// Whether the answer for `name` is signed and validates, for the DNS check of our own names.
    pub async fn security(&self, name: &str, record_type: RecordType) -> Option<Security> {
        if !self.works().await {
            return None;
        }
        match self.lookup(name, record_type).await {
            Checked::Secure(..) => Some(Security::Secure),
            Checked::Insecure => Some(Security::Insecure),
            Checked::Bogus => Some(Security::Bogus),
            Checked::Failed(_) => None,
        }
    }
}

fn valid_for(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

fn cache_key(name: &str) -> Box<str> {
    format!("{}.", name.trim_end_matches('.').to_ascii_lowercase()).into_boxed_str()
}

/// What the validating resolver says about a domain's MX records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidatedMx {
    /// Signed, and the signatures hold: the MX hosts as this answer names them, the most preferred
    /// first (`[""]` for a null MX). DANE applies to these hosts, and only to these.
    Secure(Arc<[String]>),
    /// Unsigned, or the validating lookup did not work: DANE does not apply, and the ordinary
    /// answer is used.
    Insecure,
    /// Signed, but the signatures do not validate.
    Bogus,
}

impl ValidatedMx {
    pub(crate) fn security(&self) -> Security {
        match self {
            ValidatedMx::Secure(_) => Security::Secure,
            ValidatedMx::Insecure => Security::Insecure,
            ValidatedMx::Bogus => Security::Bogus,
        }
    }
}

/// `domain`'s MX records as the validating resolver sees them, which DANE needs before anything
/// else. Asked whatever the ordinary resolver said, "no MX" included: RFC 7672 wants the MX hosts
/// themselves from the validated answer, and an answer from a resolver that does not validate is
/// what an attacker on the path forges to take DANE out of the way (security-audit-0.16.0 SMTP-3).
pub(crate) async fn validated_mx(ctx: &Context, domain: &str) -> ValidatedMx {
    let key = cache_key(domain);
    if let Some(known) = ctx.dns.dane_mx.get(&key) {
        return known;
    }
    if !ctx.validator.works().await {
        return ValidatedMx::Insecure;
    }
    let (found, ttl) = match ctx.validator.lookup(domain, RecordType::MX).await {
        Checked::Secure(data, ttl) => {
            let mut exchanges: Vec<(u16, String)> = data
                .into_iter()
                .filter_map(|data| match data {
                    RData::MX(mx) => Some((mx.preference, mx.exchange.to_ascii().trim_end_matches('.').to_owned())),
                    _ => None,
                })
                .collect();
            exchanges.sort_by_key(|(preference, _)| *preference);
            if exchanges.is_empty() {
                (ValidatedMx::Insecure, ttl)
            } else {
                (ValidatedMx::Secure(exchanges.into_iter().map(|(_, host)| host).collect()), ttl)
            }
        }
        Checked::Insecure => (ValidatedMx::Insecure, MAX_TTL),
        Checked::Bogus => (ValidatedMx::Bogus, MIN_TTL),
        Checked::Failed(error) => {
            tracing::info!(%domain, %error, "the DNSSEC lookup of the MX records failed, delivering without DANE");
            (ValidatedMx::Insecure, FAILED_TTL)
        }
    };
    ctx.dns.dane_mx.insert(key, found.clone(), valid_for(ttl));
    found
}

/// The validated TLSA records of an MX host.
pub(crate) async fn host_tlsa(ctx: &Context, host: &str) -> HostTlsa {
    let name = tlsa_name(host);
    let key = cache_key(&name);
    if let Some(known) = ctx.dns.tlsa.get(&key) {
        return known;
    }
    let (found, ttl) = match ctx.validator.lookup(&name, RecordType::TLSA).await {
        Checked::Secure(data, ttl) => {
            let records: Arc<[Tlsa]> = data
                .into_iter()
                .filter_map(|data| match data {
                    RData::TLSA(tlsa) => Some(Tlsa {
                        usage: tlsa.cert_usage.into(),
                        selector: tlsa.selector.into(),
                        matching: tlsa.matching.into(),
                        data: tlsa.cert_data,
                    }),
                    _ => None,
                })
                .collect();
            if records.is_empty() { (HostTlsa::None, ttl) } else { (HostTlsa::Records(records), ttl) }
        }
        Checked::Insecure => (HostTlsa::None, MAX_TTL),
        Checked::Bogus => (HostTlsa::Bogus, MIN_TTL),
        Checked::Failed(error) => {
            tracing::warn!(%host, %error, "the TLSA lookup failed, delivering without DANE");
            (HostTlsa::None, FAILED_TTL)
        }
    };
    ctx.dns.tlsa.insert(key, found.clone(), valid_for(ttl));
    found
}

#[cfg(test)]
mod tests {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};

    use super::*;

    struct Chain {
        root: CertificateDer<'static>,
        intermediate: CertificateDer<'static>,
        leaf: CertificateDer<'static>,
        leaf_key: KeyPair,
    }

    fn ca(name: &str) -> CertificateParams {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
    }

    fn chain(host: &str) -> Chain {
        let root_key = KeyPair::generate().unwrap();
        let root_params = ca("Test Root");
        let root = root_params.self_signed(&root_key).unwrap();
        let root_issuer = Issuer::new(root_params, root_key);
        let intermediate_key = KeyPair::generate().unwrap();
        let intermediate_params = ca("Test Intermediate");
        let intermediate = intermediate_params.signed_by(&intermediate_key, &root_issuer).unwrap();
        let intermediate_issuer = Issuer::new(intermediate_params, intermediate_key);
        let leaf_key = KeyPair::generate().unwrap();
        let leaf =
            CertificateParams::new(vec![host.to_owned()]).unwrap().signed_by(&leaf_key, &intermediate_issuer).unwrap();
        Chain { root: root.der().clone(), intermediate: intermediate.der().clone(), leaf: leaf.der().clone(), leaf_key }
    }

    fn record(usage: u8, selector: u8, matching: u8, cert: &CertificateDer<'_>) -> Tlsa {
        let selected = match selector {
            0 => cert.as_ref().to_vec(),
            _ => ParsedCertificate::try_from(cert).unwrap().subject_public_key_info().as_ref().to_vec(),
        };
        let data = match matching {
            0 => selected,
            1 => digest::digest(&digest::SHA256, &selected).as_ref().to_vec(),
            _ => digest::digest(&digest::SHA512, &selected).as_ref().to_vec(),
        };
        Tlsa { usage, selector, matching, data }
    }

    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    }

    fn names(names: &[&str]) -> Vec<ServerName<'static>> {
        names.iter().map(|name| ServerName::try_from(name.to_string()).unwrap()).collect()
    }

    #[test]
    fn dane_ee_matches_the_certificate_or_its_key_in_every_form() {
        let chain = chain("mx.example.com");
        for selector in 0..=1 {
            for matching in 0..=2 {
                let tlsa = record(3, selector, matching, &chain.leaf);
                assert!(tlsa.matches(&chain.leaf), "3 {selector} {matching}");
                assert!(!tlsa.matches(&chain.intermediate), "3 {selector} {matching} is only the leaf");
                assert_eq!(Tlsa::parse(&tlsa.to_string()), Some(tlsa.clone()), "round trip");
            }
        }
        assert_eq!(dane_ee_record(&chain.leaf), Some(record(3, 1, 1, &chain.leaf)));

        // A new certificate for the same key keeps matching `3 1 1`, not `3 0 1`.
        let renewed =
            CertificateParams::new(vec!["mx.example.com".to_owned()]).unwrap().self_signed(&chain.leaf_key).unwrap();
        assert!(record(3, 1, 1, &chain.leaf).matches(renewed.der()));
        assert!(!record(3, 0, 1, &chain.leaf).matches(renewed.der()));
    }

    #[test]
    fn only_dane_usages_with_known_parameters_are_usable() {
        let chain = chain("mx.example.com");
        assert!(record(3, 1, 1, &chain.leaf).usable());
        assert!(record(2, 0, 1, &chain.root).usable());
        assert!(!record(1, 1, 1, &chain.leaf).usable(), "PKIX-EE is not for SMTP");
        assert!(!record(0, 1, 1, &chain.root).usable(), "PKIX-TA neither");
        assert!(!Tlsa { usage: 3, selector: 2, matching: 1, data: vec![1] }.usable());
        assert!(!Tlsa { usage: 3, selector: 1, matching: 7, data: vec![1] }.usable());
        assert_eq!(Tlsa::parse("3 1 1 zz"), None);
        assert_eq!(Tlsa::parse("3 1 1 0A0b").map(|tlsa| tlsa.data), Some(vec![10, 11]));

        let records: Arc<[Tlsa]> = vec![record(1, 1, 1, &chain.leaf)].into();
        assert_eq!(Dane::of(HostTlsa::Records(records.clone())), Dane::EncryptOnly(records));
        assert_eq!(Dane::of(HostTlsa::None), Dane::Off);
    }

    #[test]
    fn the_verifier_takes_matching_certificates_and_rejects_the_rest() {
        let chain = chain("mx.example.com");
        let other = super::tests::chain("mx.example.com");
        let now = UnixTime::now();
        let provider = provider();
        let mx = names(&["mx.example.com", "example.com"]);
        let intermediates = [chain.intermediate.clone()];

        // DANE-EE: names do not matter.
        let ee = [record(3, 1, 1, &chain.leaf)];
        assert!(verify(&ee, &chain.leaf, &[], &names(&["elsewhere.example.net"]), now, &provider).is_ok());
        let error = verify(&ee, &other.leaf, &[], &mx, now, &provider).unwrap_err();
        assert!(matches!(error, rustls::Error::InvalidCertificate(CertificateError::Other(_))), "{error:?}");

        // DANE-TA: the chain from the matching certificate down, and the name.
        for anchor in [&chain.intermediate, &chain.root] {
            let ta = [record(2, 0, 1, anchor)];
            let with_root = [chain.intermediate.clone(), chain.root.clone()];
            assert!(verify(&ta, &chain.leaf, &with_root, &mx, now, &provider).is_ok());
            let wrong_name = verify(&ta, &chain.leaf, &with_root, &names(&["mx.example.net"]), now, &provider);
            assert!(
                matches!(
                    wrong_name,
                    Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext { .. }))
                ),
                "{wrong_name:?}"
            );
        }
        let ta = [record(2, 1, 1, &chain.intermediate)];
        assert!(verify(&ta, &chain.leaf, &intermediates, &mx, now, &provider).is_ok(), "the key of the TA");
        assert!(verify(&ta, &other.leaf, std::slice::from_ref(&other.intermediate), &mx, now, &provider).is_err());
        assert!(verify(&ta, &chain.leaf, &[], &mx, now, &provider).is_err(), "the TA has to come along");
        // A leaf another intermediate signed does not chain up to the named one.
        assert!(verify(&ta, &other.leaf, &intermediates, &mx, now, &provider).is_err());
        // The recipient domain counts as a name too.
        let domain_cert = super::tests::chain("example.com");
        let ta = [record(2, 0, 2, &domain_cert.intermediate)];
        assert!(
            verify(&ta, &domain_cert.leaf, std::slice::from_ref(&domain_cert.intermediate), &mx, now, &provider)
                .is_ok()
        );
        // PKIX usages are ignored, even when they match.
        let pkix = [record(1, 1, 1, &chain.leaf)];
        assert!(verify(&pkix, &chain.leaf, &[], &mx, now, &provider).is_err());
    }

    /// Asks real DNS; run with `cargo test -p uwumail-smtp live_dane -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn live_dane() {
        let validator = Validator::default();
        println!("root: {:?}", validator.lookup(".", RecordType::SOA).await);
        println!("ietf: {:?}", validator.lookup("ietf.org", RecordType::MX).await);
        assert!(validator.works().await, "signed answers validate here");
        assert_eq!(validator.security("ietf.org", RecordType::MX).await, Some(Security::Secure));
        assert_eq!(validator.security("example.com", RecordType::A).await, Some(Security::Secure));
        let host = std::env::var("UWUMAIL_DANE_HOST").unwrap_or_else(|_| "mail2.ietf.org".into());
        match validator.lookup(&tlsa_name(&host), RecordType::TLSA).await {
            Checked::Secure(records, ttl) => println!("{host}: {records:?} for {ttl} s"),
            Checked::Insecure => panic!("{host} has no secure TLSA records"),
            Checked::Bogus => panic!("the TLSA records of {host} are bogus"),
            Checked::Failed(error) => panic!("{error}"),
        }
    }
}
