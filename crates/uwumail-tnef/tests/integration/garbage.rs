//! Untrusted input: random bytes and damaged streams never make the decoder panic, and it stays
//! fast on them.

use uwumail_tnef::{IcsOptions, decode, rtf, safelinks};

use crate::fixtures;

/// xorshift64*: enough randomness for this, and the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn exercise(data: &[u8]) {
    if let Ok(message) = decode(data) {
        if let Some(meeting) = message.meeting() {
            let _ = meeting.to_ical(&IcsOptions::default());
        }
        for attachment in &message.attachments {
            if let Some(inner) = &attachment.embedded {
                let _ = inner.meeting().map(|m| m.to_ical(&IcsOptions::default()));
            }
        }
    }
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut data = base.to_vec();
    for _ in 0..=rng.below(8) {
        match rng.below(5) {
            0 if !data.is_empty() => {
                let i = rng.below(data.len());
                data[i] = rng.next() as u8;
            }
            1 if !data.is_empty() => {
                let i = rng.below(data.len());
                data[i] ^= 1 << rng.below(8);
            }
            2 => {
                let cut = rng.below(data.len() + 1);
                data.truncate(cut);
            }
            3 => {
                let i = rng.below(data.len() + 1);
                let n = rng.below(16);
                let extra = rng.bytes(n);
                data.splice(i..i, extra);
            }
            _ if data.len() >= 4 => {
                // A length or count field made huge.
                let i = rng.below(data.len() - 3);
                data[i..i + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0x7F]);
            }
            _ => {}
        }
    }
    data
}

#[test]
fn random_streams() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for _ in 0..3_000 {
        let len = rng.below(600);
        let mut data = uwumail_tnef::SIGNATURE.to_le_bytes().to_vec();
        data.extend(rng.bytes(len));
        exercise(&data);
    }
}

#[test]
fn damaged_streams() {
    let mut rng = Rng(0x5eed_1234_abcd_0002);
    let bases = [fixtures::note(), fixtures::request()];
    for round in 0..4_000 {
        let data = mutate(&mut rng, &bases[round % bases.len()]);
        exercise(&data);
    }
}

#[test]
fn random_rtf() {
    let mut rng = Rng(0x5eed_1234_abcd_0003);
    let packed = rtf::compress(fixtures::HTML_RTF);
    for _ in 0..3_000 {
        let data = mutate(&mut rng, &packed);
        if let Ok(raw) = rtf::decompress(&data, 1 << 20) {
            let _ = rtf::convert(&raw, 1 << 20);
        }
        let n = rng.below(300);
        let noise = rng.bytes(n);
        let _ = rtf::decompress(&noise, 1 << 16);
        let _ = rtf::convert(&noise, 1 << 16);
        let tokens: [&[u8]; 12] = [
            b"{",
            b"}",
            b"\\u-1?",
            b"\\'",
            b"\\htmltag1 ",
            b"\\*",
            b"\\bin99999 ",
            b"\\par",
            b"x",
            b"\\fldinst ",
            b"\\fldrslt ",
            b"\\uc5",
        ];
        let mut soup = b"{\\rtf1\\fromhtml1 ".to_vec();
        for _ in 0..rng.below(200) {
            soup.extend(tokens[rng.below(tokens.len())]);
        }
        let _ = rtf::convert(&soup, 1 << 16);
    }
}

#[test]
fn random_links() {
    let mut rng = Rng(0x5eed_1234_abcd_0004);
    let base = "see https://nam12.safelinks.protection.outlook.com/?url=https%3A%2F%2Fexample.com%2F%C3%A4&data=1 ok";
    for _ in 0..3_000 {
        let data = mutate(&mut rng, base.as_bytes());
        let text = String::from_utf8_lossy(&data);
        let _ = safelinks::unwrap_in_text(&text);
        let _ = safelinks::unwrap(&text);
    }
}
