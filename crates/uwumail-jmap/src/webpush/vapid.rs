//! The server's signature on every push (VAPID, RFC 8292): a short-lived JWT for the push
//! service's origin, signed with the server's P-256 key, whose public half the JMAP session
//! announces (RFC 9749) and a browser binds its subscription to.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde_json::json;

/// How long a token is good for; RFC 8292 allows at most 24 hours.
const LIFETIME_SECS: i64 = 12 * 3600;

pub struct Vapid {
    key: EcdsaKeyPair,
    /// The public key, uncompressed and base64url, as the session shows it and `k=` sends it.
    public_key: String,
}

impl Vapid {
    pub fn from_pkcs8(pkcs8: &[u8]) -> Option<Vapid> {
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8).ok()?;
        let public_key = B64.encode(key.public_key().as_ref());
        Some(Vapid { key, public_key })
    }

    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// The `Authorization` header for a push to `url`, signed by `subject` (a `mailto:` or
    /// `https:` address to reach the server's admin by).
    pub fn authorization(&self, url: &str, subject: &str, now: i64) -> Option<String> {
        let audience = origin(url)?;
        let header = B64.encode(json!({ "typ": "JWT", "alg": "ES256" }).to_string());
        let claims = B64.encode(json!({ "aud": audience, "exp": now + LIFETIME_SECS, "sub": subject }).to_string());
        let signing_input = format!("{header}.{claims}");
        let signature = self.key.sign(&SystemRandom::new(), signing_input.as_bytes()).ok()?;
        Some(format!("vapid t={signing_input}.{}, k={}", B64.encode(signature.as_ref()), self.public_key))
    }
}

/// The origin of an absolute URL: scheme, host and a port that is not the default.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let host = match (scheme.as_str(), host.rsplit_once(':')) {
        ("https", Some((name, "443"))) | ("http", Some((name, "80"))) => name.to_owned(),
        _ => host,
    };
    Some(format!("{scheme}://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

    #[test]
    fn the_token_is_signed_for_the_push_services_origin() {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new()).unwrap();
        let vapid = Vapid::from_pkcs8(pkcs8.as_ref()).unwrap();
        let header =
            vapid.authorization("https://push.example.net:443/send/abc?x=1", "https://mail.example.org", 1000).unwrap();
        let (token, key) = header.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(key, vapid.public_key());
        assert_eq!(B64.decode(key).unwrap().len(), 65);

        let (signed, signature) = token.rsplit_once('.').unwrap();
        let public = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, B64.decode(key).unwrap());
        public.verify(signed.as_bytes(), &B64.decode(signature).unwrap()).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(signed.split_once('.').unwrap().1).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://push.example.net");
        assert_eq!(claims["sub"], "https://mail.example.org");
        assert_eq!(claims["exp"], 1000 + LIFETIME_SECS);
    }

    #[test]
    fn origins_keep_ports_that_matter() {
        assert_eq!(origin("https://Push.Example.net/a").as_deref(), Some("https://push.example.net"));
        assert_eq!(origin("https://push.example.net:8443/a").as_deref(), Some("https://push.example.net:8443"));
        assert_eq!(origin("http://127.0.0.1:8080/a").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(origin("not a url"), None);
    }
}
