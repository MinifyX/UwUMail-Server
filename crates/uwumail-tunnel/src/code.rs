//! Pairing codes: everything a server needs to find its gateway and trust it, in one line to copy
//! from the gateway's log into the setup assistant.
//!
//! A code is `uwugw1` followed by base32 of: the number of addresses, for each one its family
//! (4 or 6), the address and the port; the SHA-256 fingerprint of the gateway's certificate; a
//! one-time token; and the first four bytes of the SHA-256 of all that, so a code damaged while
//! copying is noticed instead of failing in a confusing way.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use aws_lc_rs::digest;
use data_encoding::BASE32_NOPAD;

use crate::identity::Fingerprint;

const PREFIX: &str = "uwugw1";
const MAX_ADDRESSES: usize = 4;
const CHECK_LEN: usize = 4;

/// The secret half of a pairing code. Whoever knows it may pair with the gateway, once.
#[derive(Clone)]
pub struct Token([u8; 16]);

impl Token {
    pub fn generate() -> Token {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the system RNG failed");
        Token(bytes)
    }

    pub fn to_text(&self) -> String {
        BASE32_NOPAD.encode(&self.0).to_ascii_lowercase()
    }

    pub fn from_text(text: &str) -> Option<Token> {
        let bytes = BASE32_NOPAD.decode(text.trim().to_ascii_uppercase().as_bytes()).ok()?;
        Some(Token(bytes.try_into().ok()?))
    }

    /// Compares in constant time.
    pub fn matches(&self, other: &Token) -> bool {
        aws_lc_rs::constant_time::verify_slices_are_equal(&self.0, &other.0).is_ok()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeError {
    #[error("this is not a pairing code of a UwUMail Gateway")]
    NotACode,
    #[error("the pairing code is damaged or incomplete, please copy it again")]
    Damaged,
}

#[derive(Debug, Clone)]
pub struct PairingCode {
    /// Where the gateway listens for the tunnel, in the order to try.
    pub addresses: Vec<SocketAddr>,
    pub fingerprint: Fingerprint,
    pub token: Token,
}

impl PairingCode {
    pub fn encode(&self) -> String {
        let mut bytes = Vec::with_capacity(96);
        let addresses = &self.addresses[..self.addresses.len().min(MAX_ADDRESSES)];
        bytes.push(addresses.len() as u8);
        for address in addresses {
            match address.ip() {
                IpAddr::V4(ip) => {
                    bytes.push(4);
                    bytes.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    bytes.push(6);
                    bytes.extend_from_slice(&ip.octets());
                }
            }
            bytes.extend_from_slice(&address.port().to_be_bytes());
        }
        bytes.extend_from_slice(self.fingerprint.as_bytes());
        bytes.extend_from_slice(&self.token.0);
        let check = digest::digest(&digest::SHA256, &bytes);
        bytes.extend_from_slice(&check.as_ref()[..CHECK_LEN]);
        format!("{PREFIX}{}", BASE32_NOPAD.encode(&bytes).to_ascii_lowercase())
    }

    /// Reads a code, ignoring case, spaces, line breaks and dashes added while copying.
    pub fn parse(text: &str) -> Result<PairingCode, CodeError> {
        let cleaned: String =
            text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
        let body = cleaned.strip_prefix(&PREFIX.to_ascii_uppercase()).ok_or(CodeError::NotACode)?;
        let bytes = BASE32_NOPAD.decode(body.as_bytes()).map_err(|_| CodeError::Damaged)?;
        if bytes.len() < CHECK_LEN {
            return Err(CodeError::Damaged);
        }
        let (payload, check) = bytes.split_at(bytes.len() - CHECK_LEN);
        if digest::digest(&digest::SHA256, payload).as_ref()[..CHECK_LEN] != *check {
            return Err(CodeError::Damaged);
        }

        let mut reader = Reader(payload);
        let count = usize::from(reader.take(1)?[0]);
        if count == 0 || count > MAX_ADDRESSES {
            return Err(CodeError::Damaged);
        }
        let mut addresses = Vec::with_capacity(count);
        for _ in 0..count {
            let ip = match reader.take(1)?[0] {
                4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(reader.take(4)?).expect("length checked"))),
                6 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(reader.take(16)?).expect("length checked"))),
                _ => return Err(CodeError::Damaged),
            };
            let port = u16::from_be_bytes(reader.take(2)?.try_into().expect("length checked"));
            addresses.push(SocketAddr::new(ip, port));
        }
        let fingerprint = Fingerprint::from_bytes(reader.take(32)?.try_into().expect("length checked"));
        let token = Token(reader.take(16)?.try_into().expect("length checked"));
        if !reader.0.is_empty() {
            return Err(CodeError::Damaged);
        }
        Ok(PairingCode { addresses, fingerprint, token })
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], CodeError> {
        if self.0.len() < len {
            return Err(CodeError::Damaged);
        }
        let (taken, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(taken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PairingCode {
        PairingCode {
            addresses: vec!["192.0.2.10:443".parse().unwrap(), "[2001:db8::10]:443".parse().unwrap()],
            fingerprint: Fingerprint::from_bytes([7; 32]),
            token: Token::generate(),
        }
    }

    #[test]
    fn codes_round_trip() {
        let code = sample();
        let text = code.encode();
        assert!(text.starts_with("uwugw1"));
        assert!(text.chars().all(|c| c.is_ascii_alphanumeric()), "double-click selects the whole code");

        let parsed = PairingCode::parse(&text).unwrap();
        assert_eq!(parsed.addresses, code.addresses);
        assert_eq!(parsed.fingerprint, code.fingerprint);
        assert!(parsed.token.matches(&code.token));

        // Copied with a line break in the middle, in capitals.
        let (a, b) = text.split_at(40);
        assert!(PairingCode::parse(&format!(" {}\n{} ", a.to_uppercase(), b)).is_ok());
    }

    #[test]
    fn damaged_codes_are_noticed() {
        let text = sample().encode();
        assert_eq!(PairingCode::parse("hello").unwrap_err(), CodeError::NotACode);
        assert_eq!(PairingCode::parse(&text[..text.len() - 3]).unwrap_err(), CodeError::Damaged);

        let mut typo: Vec<char> = text.chars().collect();
        typo[20] = if typo[20] == 'a' { 'b' } else { 'a' };
        assert_eq!(PairingCode::parse(&typo.into_iter().collect::<String>()).unwrap_err(), CodeError::Damaged);
    }

    #[test]
    fn tokens_compare_and_print_safely() {
        let token = Token::generate();
        assert!(token.matches(&Token::from_text(&token.to_text()).unwrap()));
        assert!(!token.matches(&Token::generate()));
        assert_eq!(format!("{token:?}"), "Token(…)");
        assert!(Token::from_text("not base32!").is_none());
    }
}
