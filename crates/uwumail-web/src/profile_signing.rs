//! Signing Apple configuration profiles, so iPhone, iPad and Mac show them as "Verified".
//!
//! A signed profile is a CMS `SignedData` (RFC 5652) with the profile inside (attached content),
//! signed with the server's TLS certificate and key and carrying the whole chain. The structure is
//! small and fixed, so it is written here as plain DER; the signature itself comes from aws-lc-rs,
//! the crypto library the rest of the server already uses for TLS and DKIM. No `openssl` command
//! is needed at runtime, and no second RSA implementation enters the build.
//!
//! A self-signed certificate proves nothing to a phone, so with one the profile stays unsigned, as
//! before.

use aws_lc_rs::digest;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair, RSA_PKCS1_SHA256, RsaKeyPair,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Where the certificate and key in use come from: the PEM of the chain (leaf first) and of the key.
pub type ProfileKeySource = std::sync::Arc<dyn Fn() -> Option<(Vec<u8>, Vec<u8>)> + Send + Sync>;

// Object identifiers, DER-encoded without tag and length.
const OID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
const OID_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01];
const OID_CONTENT_TYPE: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x03];
const OID_MESSAGE_DIGEST: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x04];
const OID_SIGNING_TIME: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x05];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
const OID_SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];

enum Key {
    Rsa(RsaKeyPair),
    P256(EcdsaKeyPair),
    P384(EcdsaKeyPair),
}

impl Key {
    fn load(key: &PrivateKeyDer<'_>) -> Result<Key, String> {
        let der = key.secret_der();
        match key {
            PrivateKeyDer::Pkcs1(_) => RsaKeyPair::from_der(der).map(Key::Rsa).map_err(|err| err.to_string()),
            _ => RsaKeyPair::from_pkcs8(der)
                .map(Key::Rsa)
                .or_else(|_| EcdsaKeyPair::from_private_key_der(&ECDSA_P256_SHA256_ASN1_SIGNING, der).map(Key::P256))
                .or_else(|_| EcdsaKeyPair::from_private_key_der(&ECDSA_P384_SHA384_ASN1_SIGNING, der).map(Key::P384))
                .map_err(|_| "the key is neither RSA nor ECDSA P-256 or P-384".to_owned()),
        }
    }

    /// The digest algorithm, its OID and the signature algorithm OID with its parameters.
    fn algorithms(&self) -> (&'static digest::Algorithm, &'static [u8], Vec<u8>) {
        match self {
            Key::Rsa(_) => {
                (&digest::SHA256, OID_SHA256, sequence(&[oid(OID_RSA_ENCRYPTION), vec![0x05, 0x00]].concat()))
            }
            Key::P256(_) => (&digest::SHA256, OID_SHA256, sequence(&oid(OID_ECDSA_SHA256))),
            Key::P384(_) => (&digest::SHA384, OID_SHA384, sequence(&oid(OID_ECDSA_SHA384))),
        }
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let rng = SystemRandom::new();
        match self {
            Key::Rsa(key) => {
                let mut signature = vec![0; key.public_modulus_len()];
                key.sign(&RSA_PKCS1_SHA256, &rng, message, &mut signature).map_err(|_| "RSA signing failed")?;
                Ok(signature)
            }
            Key::P256(key) | Key::P384(key) => {
                Ok(key.sign(&rng, message).map_err(|_| "ECDSA signing failed")?.as_ref().to_vec())
            }
        }
    }
}

/// A certificate chain and its key, ready to sign profiles.
pub struct SigningIdentity {
    chain: Vec<Vec<u8>>,
    issuer: Vec<u8>,
    serial: Vec<u8>,
    key: Key,
}

impl SigningIdentity {
    /// Reads a PEM chain (leaf first) and its key. `Ok(None)` for a self-signed certificate, which
    /// would not make the profile any more trustworthy.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Option<SigningIdentity>, String> {
        let chain: Vec<Vec<u8>> = CertificateDer::pem_slice_iter(cert_pem)
            .map(|cert| cert.map(|cert| cert.as_ref().to_vec()))
            .collect::<Result<_, _>>()
            .map_err(|err| format!("reading the certificate: {err}"))?;
        let leaf = chain.first().ok_or("there is no certificate")?;
        let fields = CertificateFields::read(leaf).ok_or("the certificate cannot be read")?;
        if fields.issuer == fields.subject {
            return Ok(None);
        }
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|err| format!("reading the key: {err}"))?;
        let key = Key::load(&key)?;
        Ok(Some(SigningIdentity { issuer: fields.issuer, serial: fields.serial, chain, key }))
    }

    /// The profile as a CMS SignedData with the profile attached, DER-encoded.
    pub fn sign(&self, content: &[u8], now: i64) -> Result<Vec<u8>, String> {
        let (digest_algorithm, digest_oid, signature_algorithm) = self.key.algorithms();
        let digest_identifier = sequence(&oid(digest_oid));
        let content_digest = digest::digest(digest_algorithm, content);

        let mut attributes = vec![
            attribute(OID_CONTENT_TYPE, &oid(OID_DATA)),
            attribute(OID_SIGNING_TIME, &utc_time(now)),
            attribute(OID_MESSAGE_DIGEST, &tlv(0x04, content_digest.as_ref())),
        ];
        // DER sorts the members of a SET OF by their encoding.
        attributes.sort();
        let attributes = attributes.concat();
        // What is signed is the attributes as a SET; in the SignerInfo they carry the tag [0].
        let signature = self.key.sign(&tlv(0x31, &attributes))?;

        let signer_info = sequence(
            &[
                integer(&[1]),
                sequence(&[self.issuer.clone(), tlv(0x02, &self.serial)].concat()),
                digest_identifier.clone(),
                tlv(0xa0, &attributes),
                signature_algorithm,
                tlv(0x04, &signature),
            ]
            .concat(),
        );
        let encapsulated = sequence(&[oid(OID_DATA), tlv(0xa0, &tlv(0x04, content))].concat());
        let signed_data = sequence(
            &[
                integer(&[1]),
                tlv(0x31, &digest_identifier),
                encapsulated,
                tlv(0xa0, &self.chain.concat()),
                tlv(0x31, &signer_info),
            ]
            .concat(),
        );
        Ok(sequence(&[oid(OID_SIGNED_DATA), tlv(0xa0, &signed_data)].concat()))
    }
}

/// Signs a profile with the server's certificate when it is one a phone can verify. Unsigned
/// otherwise, and when signing fails, which is logged.
pub fn sign_profile(source: Option<&ProfileKeySource>, profile: Vec<u8>) -> Vec<u8> {
    let Some((cert, key)) = source.and_then(|source| source()) else { return profile };
    match SigningIdentity::from_pem(&cert, &key).and_then(|identity| match identity {
        Some(identity) => identity.sign(&profile, now()).map(Some),
        None => Ok(None),
    }) {
        Ok(Some(signed)) => signed,
        Ok(None) => profile,
        Err(err) => {
            tracing::warn!(%err, "the configuration profile could not be signed, handing it out unsigned");
            profile
        }
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ------------------------------------------------------------------------------------------------
// DER, as far as this needs it.

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = value.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes: Vec<u8> = len.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(value);
    out
}

fn sequence(value: &[u8]) -> Vec<u8> {
    tlv(0x30, value)
}

fn oid(value: &[u8]) -> Vec<u8> {
    tlv(0x06, value)
}

fn integer(value: &[u8]) -> Vec<u8> {
    tlv(0x02, value)
}

fn attribute(kind: &[u8], value: &[u8]) -> Vec<u8> {
    sequence(&[oid(kind), tlv(0x31, value)].concat())
}

/// `YYMMDDHHMMSSZ`, as CMS wants signing times before 2050.
fn utc_time(unix: i64) -> Vec<u8> {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let text = format!(
        "{:02}{month:02}{day:02}{:02}{:02}{:02}Z",
        year % 100,
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    tlv(0x17, text.as_bytes())
}

/// One DER element: its tag, its content and everything it took, tag and length included.
pub(crate) struct Der<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub raw: &'a [u8],
}

/// Reads the element at the start of `input` and returns it with what follows.
pub(crate) fn read_der(input: &[u8]) -> Option<(Der<'_>, &[u8])> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let bytes = input.get(2..2 + count)?;
        (bytes.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize), 2 + count)
    };
    let end = header.checked_add(len)?;
    let raw = input.get(..end)?;
    Some((Der { tag, content: &raw[header..], raw }, &input[end..]))
}

/// The elements inside a constructed element.
pub(crate) fn der_children(content: &[u8]) -> Option<Vec<Der<'_>>> {
    let mut rest = content;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (element, after) = read_der(rest)?;
        out.push(element);
        rest = after;
    }
    Some(out)
}

struct CertificateFields {
    serial: Vec<u8>,
    issuer: Vec<u8>,
    subject: Vec<u8>,
}

impl CertificateFields {
    fn read(cert: &[u8]) -> Option<CertificateFields> {
        let (certificate, _) = read_der(cert)?;
        let parts = der_children(certificate.content)?;
        let tbs = der_children(parts.first()?.content)?;
        // The version is optional and tagged [0].
        let skip = usize::from(tbs.first()?.tag == 0xa0);
        let serial = tbs.get(skip)?;
        let issuer = tbs.get(skip + 2)?;
        let subject = tbs.get(skip + 4)?;
        (serial.tag == 0x02 && issuer.tag == 0x30 && subject.tag == 0x30).then(|| CertificateFields {
            serial: serial.content.to_vec(),
            issuer: issuer.raw.to_vec(),
            subject: subject.raw.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    /// A CA, and a leaf it issued for `mail.example.org`, as PEM chain and key.
    fn issued(algorithm: &'static rcgen::SignatureAlgorithm) -> (String, String) {
        let ca_key = rcgen::KeyPair::generate_for(algorithm).unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Example Test CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let key = rcgen::KeyPair::generate_for(algorithm).unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["mail.example.org".to_owned()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, "mail.example.org");
        let leaf = params.signed_by(&key, &issuer).unwrap();
        (format!("{}{}", leaf.pem(), ca.pem()), key.serialize_pem())
    }

    /// Takes a signed profile apart and checks it the way a device does: the content, the chain,
    /// the digest over the content and the signature over the signed attributes.
    fn verify(signed: &[u8], content: &[u8], chain_pem: &str, rsa: bool) {
        let (info, rest) = read_der(signed).unwrap();
        assert!(rest.is_empty());
        let info = der_children(info.content).unwrap();
        assert_eq!(info[0].content, OID_SIGNED_DATA);
        let signed_data = der_children(der_children(info[1].content).unwrap()[0].content).unwrap();
        assert_eq!(signed_data[0].content, [1]);
        let encapsulated = der_children(signed_data[2].content).unwrap();
        assert_eq!(encapsulated[0].content, OID_DATA);
        let attached = der_children(encapsulated[1].content).unwrap();
        assert_eq!(attached[0].content, content, "the profile is attached as it was");

        let certificates = der_children(signed_data[3].content).unwrap();
        let chain: Vec<Vec<u8>> =
            CertificateDer::pem_slice_iter(chain_pem.as_bytes()).map(|c| c.unwrap().as_ref().to_vec()).collect();
        assert_eq!(certificates.iter().map(|c| c.raw.to_vec()).collect::<Vec<_>>(), chain, "the whole chain");

        let signer = der_children(der_children(signed_data[4].content).unwrap()[0].content).unwrap();
        let leaf = CertificateFields::read(&chain[0]).unwrap();
        let sid = der_children(signer[1].content).unwrap();
        assert_eq!((sid[0].raw, sid[1].content), (&leaf.issuer[..], &leaf.serial[..]));
        let attributes = signer[3].content;
        let digest_attribute = der_children(attributes)
            .unwrap()
            .into_iter()
            .map(|a| der_children(a.content).unwrap())
            .find(|a| a[0].content == OID_MESSAGE_DIGEST)
            .unwrap();
        let expected = digest::digest(&digest::SHA256, content);
        assert_eq!(der_children(digest_attribute[1].content).unwrap()[0].content, expected.as_ref());

        // The leaf's public key, out of its SubjectPublicKeyInfo.
        let (cert, _) = read_der(&chain[0]).unwrap();
        let tbs = der_children(der_children(cert.content).unwrap()[0].content).unwrap();
        let spki = der_children(tbs[6].content).unwrap();
        let public_key = &spki[1].content[1..];
        let signed_attributes = tlv(0x31, attributes);
        let algorithm: &dyn aws_lc_rs::signature::VerificationAlgorithm =
            if rsa { &RSA_PKCS1_2048_8192_SHA256 } else { &ECDSA_P256_SHA256_ASN1 };
        UnparsedPublicKey::new(algorithm, public_key).verify(&signed_attributes, signer[5].content).unwrap();
    }

    #[test]
    fn profiles_are_signed_with_the_whole_chain() {
        let content = b"<?xml version=\"1.0\"?><plist><dict/></plist>".to_vec();
        for (algorithm, rsa) in [(&rcgen::PKCS_ECDSA_P256_SHA256, false), (&rcgen::PKCS_RSA_SHA256, true)] {
            let (chain, key) = issued(algorithm);
            let identity = SigningIdentity::from_pem(chain.as_bytes(), key.as_bytes()).unwrap().unwrap();
            let signed = identity.sign(&content, 1_789_894_800).unwrap();
            verify(&signed, &content, &chain, rsa);
        }
    }

    #[test]
    fn self_signed_certificates_leave_profiles_unsigned() {
        let generated = rcgen::generate_simple_self_signed(vec!["mail.example.org".to_owned()]).unwrap();
        let (cert, key) = (generated.cert.pem(), generated.signing_key.serialize_pem());
        assert!(SigningIdentity::from_pem(cert.as_bytes(), key.as_bytes()).unwrap().is_none());
        let source: ProfileKeySource = std::sync::Arc::new(move || Some((cert.clone().into(), key.clone().into())));
        assert_eq!(sign_profile(Some(&source), b"plain".to_vec()), b"plain");
        assert_eq!(sign_profile(None, b"plain".to_vec()), b"plain");
    }

    #[test]
    fn times_are_written_the_cms_way() {
        assert_eq!(utc_time(784_887_151), tlv(0x17, b"941115081231Z"));
        assert_eq!(utc_time(1_789_894_800)[2..], *b"260920090000Z");
    }
}
