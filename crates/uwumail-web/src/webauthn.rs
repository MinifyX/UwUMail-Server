//! Just enough WebAuthn to use passkeys and security keys as a second factor: checking a new
//! credential and checking a login with it. Attestation is not verified (the portal asks for
//! "none"), so any authenticator the browser offers works.
//!
//! Supported keys: ES256 (P-256), EdDSA (Ed25519) and RS256, which covers every current
//! authenticator.

use aws_lc_rs::{digest, signature};
use data_encoding::BASE64URL_NOPAD;
use serde::Deserialize;

/// Who the credentials belong to: the server's hostname, used from its HTTPS origin.
#[derive(Debug, Clone)]
pub struct RelyingParty {
    pub id: String,
    pub origin: String,
}

impl RelyingParty {
    pub fn for_hostname(hostname: &str) -> RelyingParty {
        RelyingParty { id: hostname.to_owned(), origin: format!("https://{hostname}") }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub credential_id: Vec<u8>,
    /// The COSE key, stored as it came.
    pub public_key: Vec<u8>,
    pub sign_count: u32,
}

const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_ATTESTED_CREDENTIAL: u8 = 0x40;

pub fn decode(value: &str) -> Result<Vec<u8>, String> {
    BASE64URL_NOPAD.decode(value.trim_end_matches('=').as_bytes()).map_err(|_| "not base64url".to_owned())
}

pub fn encode(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(default)]
    cross_origin: bool,
}

fn check_client_data(rp: &RelyingParty, json: &[u8], kind: &str, challenge: &[u8]) -> Result<(), String> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| "the client data is not valid JSON")?;
    if data.kind != kind {
        return Err(format!("expected {kind}, got {}", data.kind));
    }
    let sent = decode(&data.challenge)?;
    if aws_lc_rs::constant_time::verify_slices_are_equal(&sent, challenge).is_err() {
        return Err("the challenge does not match".into());
    }
    if data.origin != rp.origin {
        return Err(format!("the origin {} is not {}", data.origin, rp.origin));
    }
    if data.cross_origin {
        return Err("cross-origin requests are not allowed".into());
    }
    Ok(())
}

struct AuthenticatorData<'a> {
    rp_id_hash: &'a [u8],
    flags: u8,
    sign_count: u32,
    /// Credential id and COSE key, present when a credential was created.
    attested: Option<(Vec<u8>, Vec<u8>)>,
}

fn parse_authenticator_data(data: &[u8]) -> Result<AuthenticatorData<'_>, String> {
    if data.len() < 37 {
        return Err("the authenticator data is too short".into());
    }
    let flags = data[32];
    let sign_count = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    let attested = if flags & FLAG_ATTESTED_CREDENTIAL != 0 {
        let rest = &data[37..];
        if rest.len() < 18 {
            return Err("the credential data is too short".into());
        }
        let id_len = usize::from(u16::from_be_bytes([rest[16], rest[17]]));
        let id_end = 18 + id_len;
        if rest.len() < id_end || id_len == 0 || id_len > 1023 {
            return Err("the credential id is malformed".into());
        }
        let credential_id = rest[18..id_end].to_vec();
        let (_, key_len) = cbor::read(&rest[id_end..])?;
        let public_key = rest[id_end..id_end + key_len].to_vec();
        Some((credential_id, public_key))
    } else {
        None
    };
    Ok(AuthenticatorData { rp_id_hash: &data[..32], flags, sign_count, attested })
}

fn check_rp_and_presence(rp: &RelyingParty, auth: &AuthenticatorData<'_>) -> Result<(), String> {
    let expected = digest::digest(&digest::SHA256, rp.id.as_bytes());
    if auth.rp_id_hash != expected.as_ref() {
        return Err("the credential belongs to another site".into());
    }
    if auth.flags & FLAG_USER_PRESENT == 0 {
        return Err("the authenticator did not confirm a person was present".into());
    }
    Ok(())
}

/// Checks the answer to `navigator.credentials.create()`.
pub fn verify_registration(
    rp: &RelyingParty,
    challenge: &[u8],
    client_data_json: &[u8],
    attestation_object: &[u8],
) -> Result<NewCredential, String> {
    check_client_data(rp, client_data_json, "webauthn.create", challenge)?;
    let (attestation, _) = cbor::read(attestation_object)?;
    let auth_data = attestation.get_text_key("authData").and_then(cbor::Value::as_bytes).ok_or("no authData")?;
    let auth = parse_authenticator_data(auth_data)?;
    check_rp_and_presence(rp, &auth)?;
    let (credential_id, public_key) = auth.attested.ok_or("no credential in the answer")?;
    // Refuse keys we could not check later.
    CoseKey::parse(&public_key)?;
    Ok(NewCredential { credential_id, public_key, sign_count: auth.sign_count })
}

/// Checks the answer to `navigator.credentials.get()`. Returns the new signature counter.
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    public_key: &[u8],
    stored_sign_count: u32,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
) -> Result<u32, String> {
    check_client_data(rp, client_data_json, "webauthn.get", challenge)?;
    let auth = parse_authenticator_data(authenticator_data)?;
    check_rp_and_presence(rp, &auth)?;
    let mut signed = authenticator_data.to_vec();
    signed.extend_from_slice(digest::digest(&digest::SHA256, client_data_json).as_ref());
    CoseKey::parse(public_key)?.verify(&signed, signature)?;
    // Counters only go up. Many passkeys always send 0, which says nothing.
    if (auth.sign_count != 0 || stored_sign_count != 0) && auth.sign_count <= stored_sign_count {
        return Err("the signature counter went backwards; the key may have been copied".into());
    }
    Ok(auth.sign_count)
}

enum CoseKey {
    Es256 { point: Vec<u8> },
    Ed25519 { x: Vec<u8> },
    Rs256 { n: Vec<u8>, e: Vec<u8> },
}

impl CoseKey {
    fn parse(bytes: &[u8]) -> Result<CoseKey, String> {
        let (key, _) = cbor::read(bytes)?;
        let int = |label: i64| key.get_int_key(label).and_then(cbor::Value::as_int);
        let bytes_at = |label: i64| key.get_int_key(label).and_then(cbor::Value::as_bytes).map(<[u8]>::to_vec);
        match (int(1), int(3)) {
            (Some(2), Some(-7)) => {
                let (x, y) = (bytes_at(-2).ok_or("no x")?, bytes_at(-3).ok_or("no y")?);
                if int(-1) != Some(1) || x.len() != 32 || y.len() != 32 {
                    return Err("only P-256 keys are supported".into());
                }
                let mut point = vec![0x04];
                point.extend_from_slice(&x);
                point.extend_from_slice(&y);
                Ok(CoseKey::Es256 { point })
            }
            (Some(1), Some(-8)) => {
                let x = bytes_at(-2).ok_or("no x")?;
                if int(-1) != Some(6) || x.len() != 32 {
                    return Err("only Ed25519 keys are supported".into());
                }
                Ok(CoseKey::Ed25519 { x })
            }
            (Some(3), Some(-257)) => {
                let (n, e) = (bytes_at(-1).ok_or("no modulus")?, bytes_at(-2).ok_or("no exponent")?);
                if n.len() < 256 {
                    return Err("RSA keys need at least 2048 bits".into());
                }
                Ok(CoseKey::Rs256 { n, e })
            }
            (kty, alg) => Err(format!("unsupported key type {kty:?} with algorithm {alg:?}")),
        }
    }

    fn verify(&self, message: &[u8], sig: &[u8]) -> Result<(), String> {
        let result = match self {
            CoseKey::Es256 { point } => {
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point).verify(message, sig)
            }
            CoseKey::Ed25519 { x } => signature::UnparsedPublicKey::new(&signature::ED25519, x).verify(message, sig),
            CoseKey::Rs256 { n, e } => signature::RsaPublicKeyComponents { n: n.as_slice(), e: e.as_slice() }.verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                message,
                sig,
            ),
        };
        result.map_err(|_| "the signature is not valid".to_owned())
    }
}

/// A CBOR reader for what authenticators send: definite lengths, no floats.
mod cbor {
    const MAX_DEPTH: usize = 8;
    const MAX_ITEMS: u64 = 4096;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Int(i128),
        Bytes(Vec<u8>),
        Text(String),
        Array(Vec<Value>),
        Map(Vec<(Value, Value)>),
        Simple(u8),
    }

    impl Value {
        pub fn as_bytes(&self) -> Option<&[u8]> {
            match self {
                Value::Bytes(bytes) => Some(bytes),
                _ => None,
            }
        }

        pub fn as_int(&self) -> Option<i64> {
            match self {
                Value::Int(value) => i64::try_from(*value).ok(),
                _ => None,
            }
        }

        pub fn get_text_key(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Map(entries) => {
                    entries.iter().find(|(k, _)| matches!(k, Value::Text(t) if t == key)).map(|(_, v)| v)
                }
                _ => None,
            }
        }

        pub fn get_int_key(&self, key: i64) -> Option<&Value> {
            match self {
                Value::Map(entries) => {
                    entries.iter().find(|(k, _)| matches!(k, Value::Int(i) if *i == i128::from(key))).map(|(_, v)| v)
                }
                _ => None,
            }
        }
    }

    /// Reads one item. Returns it and how many bytes it took.
    pub fn read(data: &[u8]) -> Result<(Value, usize), String> {
        let mut position = 0;
        let value = item(data, &mut position, 0)?;
        Ok((value, position))
    }

    fn take<'a>(data: &'a [u8], position: &mut usize, count: usize) -> Result<&'a [u8], String> {
        let end = position.checked_add(count).filter(|end| *end <= data.len()).ok_or("the CBOR data ends early")?;
        let slice = &data[*position..end];
        *position = end;
        Ok(slice)
    }

    fn argument(data: &[u8], position: &mut usize, info: u8) -> Result<u64, String> {
        Ok(match info {
            0..=23 => u64::from(info),
            24 => u64::from(take(data, position, 1)?[0]),
            25 => u64::from(u16::from_be_bytes(take(data, position, 2)?.try_into().expect("two bytes"))),
            26 => u64::from(u32::from_be_bytes(take(data, position, 4)?.try_into().expect("four bytes"))),
            27 => u64::from_be_bytes(take(data, position, 8)?.try_into().expect("eight bytes")),
            _ => return Err("indefinite lengths are not supported".into()),
        })
    }

    fn item(data: &[u8], position: &mut usize, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("the CBOR data is nested too deeply".into());
        }
        let initial = take(data, position, 1)?[0];
        let (major, info) = (initial >> 5, initial & 0x1f);
        let arg = argument(data, position, info)?;
        Ok(match major {
            0 => Value::Int(i128::from(arg)),
            1 => Value::Int(-1 - i128::from(arg)),
            2 => Value::Bytes(take(data, position, usize::try_from(arg).map_err(|_| "too long")?)?.to_vec()),
            3 => {
                let bytes = take(data, position, usize::try_from(arg).map_err(|_| "too long")?)?;
                Value::Text(String::from_utf8(bytes.to_vec()).map_err(|_| "text is not UTF-8")?)
            }
            4 if arg <= MAX_ITEMS => {
                Value::Array((0..arg).map(|_| item(data, position, depth + 1)).collect::<Result<_, _>>()?)
            }
            5 if arg <= MAX_ITEMS => Value::Map(
                (0..arg)
                    .map(|_| Ok((item(data, position, depth + 1)?, item(data, position, depth + 1)?)))
                    .collect::<Result<_, String>>()?,
            ),
            6 => item(data, position, depth + 1)?,
            7 if info < 24 => Value::Simple(info),
            _ => return Err("unsupported CBOR item".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    /// A tiny CBOR writer, only for building what an authenticator would send.
    fn head(major: u8, value: u64) -> Vec<u8> {
        match value {
            0..=23 => vec![(major << 5) | value as u8],
            24..=0xff => vec![(major << 5) | 24, value as u8],
            _ => {
                let mut out = vec![(major << 5) | 25];
                out.extend_from_slice(&(value as u16).to_be_bytes());
                out
            }
        }
    }
    fn int(value: i64) -> Vec<u8> {
        if value >= 0 { head(0, value as u64) } else { head(1, (-1 - value) as u64) }
    }
    fn bytes(data: &[u8]) -> Vec<u8> {
        [head(2, data.len() as u64), data.to_vec()].concat()
    }
    fn text(value: &str) -> Vec<u8> {
        [head(3, value.len() as u64), value.as_bytes().to_vec()].concat()
    }
    fn map(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
        let mut out = head(5, entries.len() as u64);
        for (key, value) in entries {
            out.extend_from_slice(key);
            out.extend_from_slice(value);
        }
        out
    }

    /// A P-256 key and its signature over the login in `round_trip` (mail.example.org, challenge 9…,
    /// counter 5), made once: generating keys and ECDSA signatures needs the aws-lc random generator, which
    /// crashes now and then in Windows test runs.
    const ES256_POINT: &str = "04d96a5756d6f645cb162d204923f8c35dfbc81f68023b450fb8804b4a80a4f50e1af5a5443c0d4c075b54e4d37750985fa8dc00104441329ba8ef6102e09ed43b";
    const ES256_SIGNATURE: &str = "304402203cd9eecd94c458d6123fe7afb4ee7964e0e8606d4de355d18e4b635d594cf6ef0220558edab798fd989bedab317e5efb60bc6406221dd0d71fd10725042f1516fa0b";

    fn unhex(value: &str) -> Vec<u8> {
        (0..value.len()).step_by(2).map(|i| u8::from_str_radix(&value[i..i + 2], 16).unwrap()).collect()
    }

    enum SoftKey {
        Es256,
        Ed25519(Ed25519KeyPair),
    }

    impl SoftKey {
        fn cose(&self) -> Vec<u8> {
            match self {
                SoftKey::Es256 => {
                    let point = unhex(ES256_POINT);
                    map(&[
                        (int(1), int(2)),
                        (int(3), int(-7)),
                        (int(-1), int(1)),
                        (int(-2), bytes(&point[1..33])),
                        (int(-3), bytes(&point[33..65])),
                    ])
                }
                SoftKey::Ed25519(pair) => map(&[
                    (int(1), int(1)),
                    (int(3), int(-8)),
                    (int(-1), int(6)),
                    (int(-2), bytes(pair.public_key().as_ref())),
                ]),
            }
        }

        fn sign(&self, message: &[u8]) -> Vec<u8> {
            match self {
                // Only valid for the login in `round_trip`.
                SoftKey::Es256 => unhex(ES256_SIGNATURE),
                SoftKey::Ed25519(pair) => pair.sign(message).as_ref().to_vec(),
            }
        }
    }

    fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        format!(r#"{{"type":"{kind}","challenge":"{}","origin":"{origin}","crossOrigin":false}}"#, encode(challenge))
            .into_bytes()
    }

    fn auth_data(rp_id: &str, flags: u8, count: u32, attested: Option<(&[u8], &[u8])>) -> Vec<u8> {
        let mut data = digest::digest(&digest::SHA256, rp_id.as_bytes()).as_ref().to_vec();
        data.push(flags);
        data.extend_from_slice(&count.to_be_bytes());
        if let Some((id, key)) = attested {
            data.extend_from_slice(&[0u8; 16]);
            data.extend_from_slice(&(id.len() as u16).to_be_bytes());
            data.extend_from_slice(id);
            data.extend_from_slice(key);
        }
        data
    }

    fn round_trip(key: SoftKey, uses_counter: bool) {
        let rp = RelyingParty::for_hostname("mail.example.org");
        let challenge = [7u8; 32];
        let credential_id = b"credential-one".to_vec();
        let cose = key.cose();
        let attestation = map(&[
            (text("fmt"), text("none")),
            (text("attStmt"), map(&[])),
            (text("authData"), bytes(&auth_data(&rp.id, 0x41, 0, Some((&credential_id, &cose))))),
        ]);
        let created = client_data("webauthn.create", &challenge, &rp.origin);
        let credential = verify_registration(&rp, &challenge, &created, &attestation).unwrap();
        assert_eq!(credential, NewCredential { credential_id, public_key: cose.clone(), sign_count: 0 });

        let login_challenge = [9u8; 32];
        let count = if uses_counter { 5 } else { 0 };
        let authenticator = auth_data(&rp.id, 0x05, count, None);
        let got = client_data("webauthn.get", &login_challenge, &rp.origin);
        let mut signed = authenticator.clone();
        signed.extend_from_slice(digest::digest(&digest::SHA256, &got).as_ref());
        let sig = key.sign(&signed);
        let verify = |challenge: &[u8], client: &[u8], stored: u32, sig: &[u8]| {
            verify_assertion(&rp, challenge, &credential.public_key, stored, client, &authenticator, sig)
        };
        assert_eq!(verify(&login_challenge, &got, 0, &sig), Ok(count));
        assert!(verify(&challenge, &got, 0, &sig).is_err(), "another challenge");
        let mut bad = sig.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(verify(&login_challenge, &got, 0, &bad).is_err(), "a changed signature");
        let phishing = client_data("webauthn.get", &login_challenge, "https://mail.example.org.evil.test");
        assert!(verify(&login_challenge, &phishing, 0, &sig).is_err(), "another origin");
        if uses_counter {
            assert!(verify(&login_challenge, &got, 5, &sig).is_err(), "a replayed counter");
        }
    }

    #[test]
    fn es256_passkeys_register_and_log_in() {
        round_trip(SoftKey::Es256, true);
    }

    #[test]
    fn ed25519_passkeys_register_and_log_in() {
        round_trip(SoftKey::Ed25519(Ed25519KeyPair::from_seed_unchecked(&[7u8; 32]).unwrap()), false);
    }

    #[test]
    fn credentials_for_other_sites_are_refused() {
        let rp = RelyingParty::for_hostname("mail.example.org");
        let challenge = [1u8; 32];
        let cose = SoftKey::Es256.cose();
        let attestation = map(&[
            (text("fmt"), text("none")),
            (text("attStmt"), map(&[])),
            (text("authData"), bytes(&auth_data("evil.test", 0x41, 0, Some((b"id", &cose))))),
        ]);
        let created = client_data("webauthn.create", &challenge, &rp.origin);
        assert!(verify_registration(&rp, &challenge, &created, &attestation).is_err());
        assert!(cbor::read(&[0x5f]).is_err(), "indefinite lengths");
        assert!(cbor::read(&[0x59, 0xff]).is_err(), "cut off");
    }
}
