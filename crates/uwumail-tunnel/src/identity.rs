//! The certificate each side presents in the tunnel: self-signed, pinned by its SHA-256
//! fingerprint, so no certificate authority is involved and nothing ever expires.

use std::fmt;
use std::str::FromStr;

use aws_lc_rs::digest;
use data_encoding::BASE64;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::TunnelError;

/// The name in both certificates. What counts is the pinned fingerprint.
pub(crate) const CERTIFICATE_NAME: &str = "uwumail-gateway";

/// A certificate and its private key.
#[derive(Clone)]
pub struct Identity {
    certificate: Vec<u8>,
    key: Vec<u8>,
}

impl Identity {
    /// A new Ed25519 key with a self-signed certificate.
    pub fn generate() -> Result<Identity, TunnelError> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let mut params = rcgen::CertificateParams::new(vec![CERTIFICATE_NAME.to_owned()])?;
        params.distinguished_name.push(rcgen::DnType::CommonName, "UwUMail tunnel");
        let certificate = params.self_signed(&key)?;
        Ok(Identity { certificate: certificate.der().to_vec(), key: key.serialize_der() })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.certificate)
    }

    pub(crate) fn certificate(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.certificate.clone())
    }

    pub(crate) fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity").field("fingerprint", &self.fingerprint()).finish_non_exhaustive()
    }
}

/// Stored as base64 DER: `{"certificate": "…", "key": "…"}`.
#[derive(Serialize, Deserialize)]
struct StoredIdentity {
    certificate: String,
    key: String,
}

impl Serialize for Identity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StoredIdentity { certificate: BASE64.encode(&self.certificate), key: BASE64.encode(&self.key) }
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = StoredIdentity::deserialize(deserializer)?;
        let decode = |value: &str| BASE64.decode(value.as_bytes()).map_err(serde::de::Error::custom);
        Ok(Identity { certificate: decode(&stored.certificate)?, key: decode(&stored.key)? })
    }
}

/// SHA-256 of a certificate in DER form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of(certificate: &[u8]) -> Fingerprint {
        let hash = digest::digest(&digest::SHA256, certificate);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(hash.as_ref());
        Fingerprint(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Fingerprint {
        Fingerprint(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({}…)", hex::encode(&self.0[..6]))
    }
}

impl FromStr for Fingerprint {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(value.trim()).map_err(|_| format!("'{value}' is not a fingerprint"))?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| format!("'{value}' is not a SHA-256 fingerprint"))?;
        Ok(Fingerprint(bytes))
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_unique_and_survive_storage() {
        let first = Identity::generate().unwrap();
        let second = Identity::generate().unwrap();
        assert_ne!(first.fingerprint(), second.fingerprint());

        let stored = serde_json::to_string(&first).unwrap();
        let loaded: Identity = serde_json::from_str(&stored).unwrap();
        assert_eq!(loaded.fingerprint(), first.fingerprint());
        assert!(!format!("{first:?}").contains(&BASE64.encode(&first.key)));
    }

    #[test]
    fn fingerprints_round_trip_as_hex() {
        let fingerprint = Identity::generate().unwrap().fingerprint();
        assert_eq!(fingerprint.to_string().parse::<Fingerprint>().unwrap(), fingerprint);
        assert!("abc".parse::<Fingerprint>().is_err());
        assert!("00".repeat(31).parse::<Fingerprint>().is_err());
    }
}
