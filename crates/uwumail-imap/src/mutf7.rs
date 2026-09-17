//! Modified UTF-7 (RFC 3501, 5.1.3): how IMAP clients without UTF8=ACCEPT write mailbox names.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|unit| unit.to_be_bytes()).collect();
        out.push('&');
        for chunk in bytes.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            let chars = chunk.len() + 1;
            for i in 0..chars {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            }
        }
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        match c {
            '&' => {
                flush(&mut pending, &mut out);
                out.push_str("&-");
            }
            ' '..='~' => {
                flush(&mut pending, &mut out);
                out.push(c);
            }
            _ => {
                let mut units = [0u16; 2];
                pending.extend_from_slice(c.encode_utf16(&mut units));
            }
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// `None` when the text is not valid modified UTF-7.
pub fn decode(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('-')?;
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else {
            let mut bits = 0u32;
            let mut count = 0;
            let mut bytes = Vec::new();
            for b in encoded.bytes() {
                let value = ALPHABET.iter().position(|&a| a == b)? as u32;
                bits = (bits << 6) | value;
                count += 6;
                if count >= 8 {
                    count -= 8;
                    bytes.push((bits >> count) as u8);
                    bits &= (1 << count) - 1;
                }
            }
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes.chunks(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();
            out.push_str(&String::from_utf16(&units).ok()?);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (plain, encoded) in [
            ("Entwürfe", "Entw&APw-rfe"),
            ("Katzen & Hunde", "Katzen &- Hunde"),
            ("日本語", "&ZeVnLIqe-"),
            ("🐱 Nyu", "&2D3cMQ- Nyu"),
            ("INBOX", "INBOX"),
        ] {
            assert_eq!(encode(plain), encoded, "{plain}");
            assert_eq!(decode(encoded).as_deref(), Some(plain), "{encoded}");
        }
        assert_eq!(decode("&Jjo!"), None);
    }
}
