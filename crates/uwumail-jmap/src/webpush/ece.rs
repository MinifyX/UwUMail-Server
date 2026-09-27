//! Message encryption for Web Push (RFC 8291) in the `aes128gcm` content coding (RFC 8188): what a
//! push service carries is sealed for the device, which alone holds the private key and the auth
//! secret it gave with the subscription.

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, ECDH_P256, PrivateKey, UnparsedPublicKey};
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};

/// The record size announced in the header. One record carries the whole message.
const RECORD_SIZE: u32 = 4096;
/// The AEAD tag and the padding delimiter take this much of a record.
const OVERHEAD: usize = 16 + 1;
/// The header in front of the record: salt, record size, key id length and our public key.
const HEADER: usize = 16 + 4 + 1 + PUBLIC_KEY_LEN;
/// Push services take at least 4096 bytes of body (RFC 8030, 7.2), header and record together.
const MAX_BODY: usize = 4096;
/// An uncompressed P-256 point: 0x04, then x and y.
pub const PUBLIC_KEY_LEN: usize = 65;
/// The auth secret a device hands out (RFC 8291, section 3.2).
pub const AUTH_SECRET_LEN: usize = 16;
/// The most a message may be, so that it fits into one record and every push service takes it.
pub const MAX_PLAINTEXT: usize = MAX_BODY - HEADER - OVERHEAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EceError {
    /// The device's key or auth secret is not what RFC 8291 asks for.
    BadKeys,
    TooLarge,
    Crypto,
}

impl std::fmt::Display for EceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EceError::BadKeys => "the subscription's keys are not usable",
            EceError::TooLarge => "the message is too large for a push",
            EceError::Crypto => "encrypting the message failed",
        })
    }
}

/// Whether a device's public key and auth secret are shaped right (RFC 8291, section 3).
pub fn keys_look_right(p256dh: &[u8], auth: &[u8]) -> bool {
    p256dh.len() == PUBLIC_KEY_LEN && p256dh[0] == 0x04 && auth.len() == AUTH_SECRET_LEN
}

/// Encrypts `plaintext` for the device with public key `ua_public` and `auth` secret, with a fresh
/// key pair and salt.
pub fn encrypt(plaintext: &[u8], ua_public: &[u8], auth: &[u8]) -> Result<Vec<u8>, EceError> {
    let key = PrivateKey::generate(&ECDH_P256).map_err(|_| EceError::Crypto)?;
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| EceError::Crypto)?;
    encrypt_with(plaintext, ua_public, auth, &key, &salt)
}

/// The same with a given key pair and salt: what the test vector of RFC 8291 needs.
pub(crate) fn encrypt_with(
    plaintext: &[u8],
    ua_public: &[u8],
    auth: &[u8],
    as_private: &PrivateKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, EceError> {
    if !keys_look_right(ua_public, auth) {
        return Err(EceError::BadKeys);
    }
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(EceError::TooLarge);
    }
    let as_public = as_private.compute_public_key().map_err(|_| EceError::Crypto)?;
    let as_public = as_public.as_ref();
    let ecdh_secret =
        agreement::agree(as_private, UnparsedPublicKey::new(&ECDH_P256, ua_public), EceError::BadKeys, |secret| {
            Ok(secret.to_vec())
        })?;
    // IKM = HKDF(auth secret, ECDH secret, "WebPush: info" || 0x00 || ua_public || as_public, 32)
    let mut ikm = [0u8; 32];
    Salt::new(HKDF_SHA256, auth)
        .extract(&ecdh_secret)
        .expand(&[b"WebPush: info\0", ua_public, as_public], Len(32))
        .and_then(|okm| okm.fill(&mut ikm))
        .map_err(|_| EceError::Crypto)?;
    // The content encryption key and nonce of RFC 8188, from the salt.
    let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 16];
    prk.expand(&[b"Content-Encoding: aes128gcm\0"], Len(16))
        .and_then(|okm| okm.fill(&mut cek))
        .map_err(|_| EceError::Crypto)?;
    let mut nonce = [0u8; 12];
    prk.expand(&[b"Content-Encoding: nonce\0"], Len(12))
        .and_then(|okm| okm.fill(&mut nonce))
        .map_err(|_| EceError::Crypto)?;

    // The only record is the last one: the delimiter 0x02, no padding.
    let mut record = Vec::with_capacity(plaintext.len() + OVERHEAD);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| EceError::Crypto)?);
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut record)
        .map_err(|_| EceError::Crypto)?;

    // Header: salt, record size, the key id (our public key) with its length, then the record.
    let mut out = Vec::with_capacity(16 + 4 + 1 + as_public.len() + record.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(as_public.len() as u8);
    out.extend_from_slice(as_public);
    out.extend_from_slice(&record);
    Ok(out)
}

struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

    /// The receiving side, as a browser does it: for the tests only.
    pub fn decrypt(message: &[u8], ua_private: &PrivateKey, auth: &[u8]) -> Option<Vec<u8>> {
        let salt = message.get(..16)?;
        let id_len = *message.get(20)? as usize;
        let as_public = message.get(21..21 + id_len)?;
        let record = message.get(21 + id_len..)?;
        let ua_public = ua_private.compute_public_key().ok()?;
        let secret =
            agreement::agree(ua_private, UnparsedPublicKey::new(&ECDH_P256, as_public), (), |s| Ok(s.to_vec())).ok()?;
        let mut ikm = [0u8; 32];
        Salt::new(HKDF_SHA256, auth)
            .extract(&secret)
            .expand(&[b"WebPush: info\0", ua_public.as_ref(), as_public], Len(32))
            .ok()?
            .fill(&mut ikm)
            .ok()?;
        let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
        let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(&[b"Content-Encoding: aes128gcm\0"], Len(16)).ok()?.fill(&mut cek).ok()?;
        prk.expand(&[b"Content-Encoding: nonce\0"], Len(12)).ok()?.fill(&mut nonce).ok()?;
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).ok()?);
        let mut buffer = record.to_vec();
        let plain = key.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buffer).ok()?;
        let end = plain.iter().rposition(|byte| *byte != 0)?;
        (plain[end] == 0x02).then(|| plain[..end].to_vec())
    }

    /// RFC 8291, Appendix A.
    #[test]
    fn matches_the_example_of_rfc_8291() {
        let plaintext = b"When I grow up, I want to be a watermelon";
        let as_private = PrivateKey::from_private_key(
            &ECDH_P256,
            &B64.decode("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw").unwrap(),
        )
        .unwrap();
        let ua_public = B64
            .decode("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4")
            .unwrap();
        let auth = B64.decode("BTBZMqHH6r4Tts7J_aSIgg").unwrap();
        let salt: [u8; 16] = B64.decode("DGv6ra1nlYgDCS1FRnbzlw").unwrap().try_into().unwrap();
        let message = encrypt_with(plaintext, &ua_public, &auth, &as_private, &salt).unwrap();
        assert_eq!(
            B64.encode(&message),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );

        let ua_private = PrivateKey::from_private_key(
            &ECDH_P256,
            &B64.decode("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94").unwrap(),
        )
        .unwrap();
        assert_eq!(decrypt(&message, &ua_private, &auth).unwrap(), plaintext);
    }

    #[test]
    fn every_message_gets_its_own_key_and_salt() {
        let device = PrivateKey::generate(&ECDH_P256).unwrap();
        let public = device.compute_public_key().unwrap();
        let auth = [7u8; 16];
        let first = encrypt(b"{}", public.as_ref(), &auth).unwrap();
        let second = encrypt(b"{}", public.as_ref(), &auth).unwrap();
        assert_ne!(first[..16], second[..16]);
        assert_eq!(decrypt(&first, &device, &auth).unwrap(), b"{}");
        assert_eq!(decrypt(&second, &device, &auth).unwrap(), b"{}");
        assert_eq!(decrypt(&first, &device, &[8u8; 16]), None, "the auth secret is needed");
    }

    #[test]
    fn refuses_bad_keys_and_large_messages() {
        let device = PrivateKey::generate(&ECDH_P256).unwrap();
        let public = device.compute_public_key().unwrap();
        assert_eq!(encrypt(b"{}", &public.as_ref()[1..], &[0; 16]), Err(EceError::BadKeys));
        assert_eq!(encrypt(b"{}", public.as_ref(), &[0; 8]), Err(EceError::BadKeys));
        let mut not_on_the_curve = public.as_ref().to_vec();
        not_on_the_curve[40] ^= 1;
        assert_eq!(encrypt(b"{}", &not_on_the_curve, &[0; 16]), Err(EceError::BadKeys));
        assert_eq!(encrypt(&vec![b'x'; MAX_PLAINTEXT + 1], public.as_ref(), &[0; 16]), Err(EceError::TooLarge));
        let largest = encrypt(&vec![b'x'; MAX_PLAINTEXT], public.as_ref(), &[0; 16]).unwrap();
        assert_eq!(largest.len(), 4096, "what every push service takes");
    }
}
