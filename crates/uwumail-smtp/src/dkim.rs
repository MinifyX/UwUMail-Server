//! DKIM keys for hosted domains and signing of outgoing mail.
//!
//! Every domain gets two keys: RSA-2048 for compatibility and Ed25519 (RFC 8463)
//! for the future. Mail is signed with both.

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use mail_auth::common::crypto::{Ed25519Key, RsaKey, Sha256};
use mail_auth::common::headers::HeaderWriter;
use mail_auth::dkim::DkimSigner;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use uwumail_store::{DkimKey, DkimKeyAlgorithm, Store};

use crate::SmtpError;

/// Headers covered by our signatures, in the order they are listed.
const SIGNED_HEADERS: &[&str] = &[
    "From",
    "Reply-To",
    "Subject",
    "Date",
    "Message-ID",
    "To",
    "Cc",
    "In-Reply-To",
    "References",
    "MIME-Version",
    "Content-Type",
    "Content-Transfer-Encoding",
    "List-Unsubscribe",
    "List-Unsubscribe-Post",
];

pub struct GeneratedKey {
    pub selector: String,
    pub algorithm: DkimKeyAlgorithm,
    pub private_key: Vec<u8>,
    pub public_key: String,
}

/// Creates an RSA and an Ed25519 key. `tag` makes selectors unique per rotation, e.g. `202609`.
pub fn generate_keys(tag: &str) -> Result<Vec<GeneratedKey>, SmtpError> {
    let dkim_err = |err: &dyn std::fmt::Display| SmtpError::Dkim(err.to_string());

    let rsa = KeyPair::generate(KeySize::Rsa2048).map_err(|e| dkim_err(&e))?;
    let rsa_private = rsa.as_der().map_err(|e| dkim_err(&e))?.as_ref().to_vec();
    let rsa_public = rsa.public_key().as_der().map_err(|e| dkim_err(&e))?.as_ref().to_vec();

    let ed_private = Ed25519Key::generate_pkcs8().map_err(|e| dkim_err(&e))?;
    let ed_public = Ed25519Key::from_pkcs8_der(&ed_private).map_err(|e| dkim_err(&e))?.public_key();

    Ok(vec![
        GeneratedKey {
            selector: format!("uwu{tag}r"),
            algorithm: DkimKeyAlgorithm::RsaSha256,
            private_key: rsa_private,
            public_key: BASE64.encode(rsa_public),
        },
        GeneratedKey {
            selector: format!("uwu{tag}e"),
            algorithm: DkimKeyAlgorithm::Ed25519Sha256,
            private_key: ed_private,
            public_key: BASE64.encode(ed_public),
        },
    ])
}

/// Makes sure a domain has signing keys and returns them.
pub async fn ensure_domain_keys(store: &Store, domain: &str) -> Result<Vec<DkimKey>, SmtpError> {
    let existing = store.dkim_keys(domain).await?;
    if !existing.is_empty() {
        return Ok(existing);
    }
    let tag = current_tag();
    let keys = tokio::task::spawn_blocking(move || generate_keys(&tag))
        .await
        .map_err(|err| SmtpError::Dkim(err.to_string()))??;
    for key in keys {
        store.add_dkim_key(domain, &key.selector, key.algorithm, key.private_key, key.public_key, true).await?;
    }
    Ok(store.dkim_keys(domain).await?)
}

/// Starts a rotation: new keys that only sign after `Store::activate_dkim_keys`, once their
/// DNS records are published. Selectors carry the month, plus a letter for another rotation
/// in the same month.
pub async fn prepare_rotation(store: &Store, domain: &str) -> Result<Vec<DkimKey>, SmtpError> {
    let existing = store.dkim_keys(domain).await?;
    if existing.iter().any(|key| key.state() == uwumail_store::DkimKeyState::Pending) {
        return Ok(existing);
    }
    let tag = rotation_tag(&current_tag(), &existing)?;
    let keys = tokio::task::spawn_blocking(move || generate_keys(&tag))
        .await
        .map_err(|err| SmtpError::Dkim(err.to_string()))??;
    for key in keys {
        store.add_dkim_key(domain, &key.selector, key.algorithm, key.private_key, key.public_key, false).await?;
    }
    Ok(store.dkim_keys(domain).await?)
}

/// The first of "202609", "202609b", "202609c", ... that no existing selector uses.
fn rotation_tag(month: &str, existing: &[DkimKey]) -> Result<String, SmtpError> {
    std::iter::once(month.to_owned())
        .chain(('b'..='z').map(|letter| format!("{month}{letter}")))
        .find(|tag| {
            let taken = [format!("uwu{tag}r"), format!("uwu{tag}e")];
            !existing.iter().any(|key| taken.contains(&key.selector))
        })
        .ok_or_else(|| SmtpError::Dkim("too many key rotations this month".into()))
}

/// Year and month, used in selectors.
fn current_tag() -> String {
    let days = crate::now() / 86_400;
    // Civil-from-days (Howard Hinnant), good for any date after 1970.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}")
}

/// Signs a message with the newest active key of each algorithm.
/// Returns the DKIM-Signature headers to put in front of the message.
pub fn sign(raw: &[u8], keys: &[DkimKey]) -> Result<String, SmtpError> {
    let mut headers = String::new();
    for algorithm in [DkimKeyAlgorithm::Ed25519Sha256, DkimKeyAlgorithm::RsaSha256] {
        let Some(key) = keys.iter().find(|k| k.active && k.algorithm == algorithm) else {
            continue;
        };
        let signature = match algorithm {
            DkimKeyAlgorithm::RsaSha256 => {
                let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.private_key.as_slice()));
                let pk = RsaKey::<Sha256>::from_key_der(der).map_err(|e| SmtpError::Dkim(e.to_string()))?;
                DkimSigner::from_key(pk)
                    .domain(&key.domain)
                    .selector(&key.selector)
                    .headers(SIGNED_HEADERS.iter().copied())
                    .sign(raw)
            }
            DkimKeyAlgorithm::Ed25519Sha256 => {
                let pk = Ed25519Key::from_pkcs8_der(&key.private_key).map_err(|e| SmtpError::Dkim(e.to_string()))?;
                DkimSigner::from_key(pk)
                    .domain(&key.domain)
                    .selector(&key.selector)
                    .headers(SIGNED_HEADERS.iter().copied())
                    .sign(raw)
            }
        }
        .map_err(|err| SmtpError::Dkim(err.to_string()))?;
        headers.push_str(&signature.to_header());
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_use_year_and_month() {
        let tag = current_tag();
        assert_eq!(tag.len(), 6);
        assert!(tag.starts_with("20"));
    }

    #[test]
    fn generated_keys_sign() {
        let keys: Vec<DkimKey> = generate_keys("202609")
            .unwrap()
            .into_iter()
            .map(|k| DkimKey {
                id: 0,
                domain: "example.de".into(),
                selector: k.selector,
                algorithm: k.algorithm,
                private_key: k.private_key,
                public_key: k.public_key,
                active: true,
                created_at: 0,
                retired_at: None,
            })
            .collect();
        let headers = sign(b"From: mini@example.de\r\nSubject: hi\r\n\r\nhi\r\n", &keys).unwrap();
        assert_eq!(headers.matches("DKIM-Signature:").count(), 2);
        assert!(headers.contains("s=uwu202609e"));
        assert!(headers.contains("a=rsa-sha256"));
    }
}
