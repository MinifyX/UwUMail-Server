//! JSON Web Tokens, as far as OpenID Connect needs them: ID tokens this server signs for mail apps
//! (ES256, docs/oauth.md), and ID tokens of another provider checked against its published keys
//! when someone logs in there (docs/login-oidc-ldap.md).

use aws_lc_rs::digest;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{self, EcdsaKeyPair, KeyPair, RsaPublicKeyComponents, UnparsedPublicKey};
use data_encoding::BASE64URL_NOPAD;
use serde_json::{Map, Value, json};

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

fn unb64(value: &str) -> Option<Vec<u8>> {
    BASE64URL_NOPAD.decode(value.trim_end_matches('=').as_bytes()).ok()
}

/// The key this server signs ID tokens with.
pub struct SigningKey {
    pair: EcdsaKeyPair,
    kid: String,
}

impl SigningKey {
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<SigningKey, String> {
        let pair = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8)
            .map_err(|_| "the OAuth signing key cannot be read".to_owned())?;
        // A stable name for the key: the start of its public key's hash.
        let hash = digest::digest(&digest::SHA256, pair.public_key().as_ref());
        let kid = b64(&hash.as_ref()[..12]);
        Ok(SigningKey { pair, kid })
    }

    /// The public key as a JWK (RFC 7517, 7518 section 6.2).
    pub fn jwk(&self) -> Value {
        // Uncompressed point: 0x04, then x and y of 32 bytes each.
        let point = self.pair.public_key().as_ref();
        json!({
            "kty": "EC",
            "crv": "P-256",
            "x": b64(&point[1..33]),
            "y": b64(&point[33..65]),
            "use": "sig",
            "alg": "ES256",
            "kid": self.kid,
        })
    }

    /// A signed JWT with these claims (RFC 7519, JWS compact form).
    pub fn sign(&self, claims: &Value) -> Result<String, String> {
        let header = json!({ "alg": "ES256", "typ": "JWT", "kid": self.kid });
        let input = format!("{}.{}", b64(header.to_string().as_bytes()), b64(claims.to_string().as_bytes()));
        let signature = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_| "signing the ID token failed".to_owned())?;
        Ok(format!("{input}.{}", b64(signature.as_ref())))
    }
}

/// A JWT split into its parts, before anything about it is believed.
pub struct Unverified {
    pub header: Map<String, Value>,
    pub claims: Map<String, Value>,
    signed: String,
    signature: Vec<u8>,
}

pub fn parse(token: &str) -> Result<Unverified, String> {
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("the ID token is not a signed JWT".into());
    };
    let object = |part: &str| -> Result<Map<String, Value>, String> {
        let bytes = unb64(part).ok_or("the ID token is not base64url")?;
        match serde_json::from_slice(&bytes) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err("the ID token is not JSON".into()),
        }
    };
    Ok(Unverified {
        header: object(header)?,
        claims: object(claims)?,
        signed: format!("{header}.{claims}"),
        signature: unb64(signature).ok_or("the ID token's signature is not base64url")?,
    })
}

impl Unverified {
    pub fn alg(&self) -> &str {
        self.header.get("alg").and_then(Value::as_str).unwrap_or_default()
    }

    pub fn kid(&self) -> Option<&str> {
        self.header.get("kid").and_then(Value::as_str)
    }

    /// Checks the signature against one key of a JWK set. The algorithm has to be one of the
    /// asymmetric ones and fit the key: `none` and HMAC never pass.
    pub fn verify_with(&self, jwk: &Value) -> bool {
        let field = |name: &str| jwk.get(name).and_then(Value::as_str).and_then(unb64);
        let message = self.signed.as_bytes();
        let kty = jwk.get("kty").and_then(Value::as_str).unwrap_or_default();
        if let Some(alg) = jwk.get("alg").and_then(Value::as_str)
            && alg != self.alg()
        {
            return false;
        }
        match (self.alg(), kty) {
            ("RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512", "RSA") => {
                let (Some(n), Some(e)) = (field("n"), field("e")) else { return false };
                let parameters: &signature::RsaParameters = match self.alg() {
                    "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                    "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
                    "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                    "PS256" => &signature::RSA_PSS_2048_8192_SHA256,
                    "PS384" => &signature::RSA_PSS_2048_8192_SHA384,
                    _ => &signature::RSA_PSS_2048_8192_SHA512,
                };
                RsaPublicKeyComponents { n: &n, e: &e }.verify(parameters, message, &self.signature).is_ok()
            }
            ("ES256" | "ES384", "EC") => {
                let (Some(x), Some(y)) = (field("x"), field("y")) else { return false };
                let (algorithm, crv, size): (&signature::EcdsaVerificationAlgorithm, _, _) = match self.alg() {
                    "ES256" => (&signature::ECDSA_P256_SHA256_FIXED, "P-256", 32),
                    _ => (&signature::ECDSA_P384_SHA384_FIXED, "P-384", 48),
                };
                if jwk.get("crv").and_then(Value::as_str) != Some(crv) || x.len() != size || y.len() != size {
                    return false;
                }
                let mut point = vec![4u8];
                point.extend_from_slice(&x);
                point.extend_from_slice(&y);
                UnparsedPublicKey::new(algorithm, point).verify(message, &self.signature).is_ok()
            }
            ("EdDSA", "OKP") => {
                let Some(x) = field("x") else { return false };
                if jwk.get("crv").and_then(Value::as_str) != Some("Ed25519") {
                    return false;
                }
                UnparsedPublicKey::new(&signature::ED25519, x).verify(message, &self.signature).is_ok()
            }
            _ => false,
        }
    }

    /// Checks the signature against a JWK set: the key named by `kid`, or each fitting key when the
    /// token names none.
    pub fn verify(&self, jwks: &Value) -> bool {
        let keys = jwks.get("keys").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        keys.iter()
            .filter(|key| key.get("use").and_then(Value::as_str).is_none_or(|usage| usage == "sig"))
            .filter(|key| match self.kid() {
                Some(kid) => key.get("kid").and_then(Value::as_str) == Some(kid),
                None => true,
            })
            .any(|key| self.verify_with(key))
    }

    /// Whether the JWK set holds a key the token names, to know when to fetch the set again.
    pub fn key_known(&self, jwks: &Value) -> bool {
        let Some(kid) = self.kid() else { return true };
        let keys = jwks.get("keys").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        keys.iter().any(|key| key.get("kid").and_then(Value::as_str) == Some(kid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, Ed25519KeyPair};

    #[test]
    fn signed_tokens_check_out_with_the_published_key() {
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new()).unwrap();
        let key = SigningKey::from_pkcs8(pkcs8.as_ref()).unwrap();
        let token = key.sign(&json!({ "sub": "7", "aud": "app" })).unwrap();
        let parsed = parse(&token).unwrap();
        assert_eq!(parsed.alg(), "ES256");
        assert_eq!(parsed.claims["sub"], "7");
        let jwks = json!({ "keys": [key.jwk()] });
        assert!(parsed.verify(&jwks));
        assert!(parsed.key_known(&jwks));

        // Another key, a changed claim, or "none" do not pass.
        let other = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new()).unwrap();
        let mut other = SigningKey::from_pkcs8(other.as_ref()).unwrap().jwk();
        other["kid"] = key.jwk()["kid"].clone();
        assert!(!parsed.verify(&json!({ "keys": [other] })));
        let parts: Vec<&str> = token.split('.').collect();
        let forged = format!("{}.{}.{}", parts[0], b64(br#"{"sub":"1","aud":"app"}"#), parts[2]);
        assert!(!parse(&forged).unwrap().verify(&jwks));
        let unsigned = format!("{}.{}.", b64(br#"{"alg":"none"}"#), parts[1]);
        assert!(!parse(&unsigned).unwrap().verify(&jwks));
    }

    #[test]
    fn eddsa_keys_of_other_providers() {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let input = format!("{}.{}", b64(br#"{"alg":"EdDSA"}"#), b64(br#"{"sub":"x"}"#));
        let token = format!("{input}.{}", b64(pair.sign(input.as_bytes()).as_ref()));
        let jwk = json!({ "kty": "OKP", "crv": "Ed25519", "x": b64(pair.public_key().as_ref()) });
        assert!(parse(&token).unwrap().verify(&json!({ "keys": [jwk] })));
    }
}
