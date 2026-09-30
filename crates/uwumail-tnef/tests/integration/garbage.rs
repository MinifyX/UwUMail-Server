//! Untrusted input: random bytes and damaged streams never make the decoder panic, and it stays
//! fast on them.

use std::time::{Duration, Instant};

use uwumail_tnef::builder::{Props, Tnef};
use uwumail_tnef::mapi::{self, IID_IMESSAGE};
use uwumail_tnef::{IcsOptions, Message, decode, html_to_text, rtf, safelinks};

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

#[test]
fn html_text_stays_linear() {
    // Every `<style>` and every `&` once looked through all the rest of the document.
    let styles = "<style></style>x".repeat(60_000);
    let text = uwumail_tnef::html_to_text(&styles);
    assert_eq!(text.len(), 60_000);
    let ampersands = "&".repeat(1 << 20);
    assert_eq!(uwumail_tnef::html_to_text(&ampersands).len(), 1 << 20);
    let mut rng = Rng(0x5eed_1234_abcd_0005);
    let base = "<p>A&amp;B &#x263A;</p><STYLE>p{}</STYLE><!-- c --><br>ä &bogus; <scrIpt>x</script";
    for _ in 0..3_000 {
        let data = mutate(&mut rng, base.as_bytes());
        let _ = uwumail_tnef::html_to_text(&String::from_utf8_lossy(&data));
    }
}

/// Generous: linear work on a few megabytes takes milliseconds, quadratic work minutes.
const FAST: Duration = Duration::from_secs(10);

#[test]
fn html_bodies_stay_linear() {
    // A stream whose only body is PR_HTML: its text is made of the HTML while decoding.
    let styles = "<style></style><STYLE></Style>".repeat((2 << 20) / 30);
    let ampersands = "&".repeat(2 << 20);
    for html in [&styles, &ampersands] {
        let mut t = Tnef::new();
        t.message_props(&Props::new().binary(mapi::PR_HTML, html.as_bytes()));
        let data = t.build();
        let start = Instant::now();
        let message = decode(&data).unwrap();
        assert!(start.elapsed() < FAST, "{:?}", start.elapsed());
        assert!(message.complete);
        assert_eq!(message.body.text.is_some(), html.starts_with('&'));
        assert!(html_to_text(html).len() <= html.len());
    }
}

/// Compressed RTF of `head` and then `unit` `times` over, each 17 bytes a reference to the unit
/// before: a few bytes that unpack to megabytes, as well as LZFu allows.
fn packed_repeat(head: &[u8], unit: &[u8], times: usize) -> Vec<u8> {
    const PREBUF_LEN: usize = 207;
    let literal = [head, unit].concat();
    let raw_size = head.len() + unit.len() * times;
    let mut body = Vec::new();
    let mut tokens: Vec<Option<u16>> = literal.iter().map(|_| None).collect();
    let mut written = literal.len();
    while written < raw_size {
        let len = (raw_size - written).min(17);
        let offset = (PREBUF_LEN + written - unit.len()) % 4096;
        tokens.push(Some(((offset << 4) | (len - 2)) as u16));
        written += len;
    }
    // The end: a reference to where the next byte would go.
    tokens.push(Some((((PREBUF_LEN + written) % 4096) << 4) as u16));
    let mut literals = literal.iter();
    for chunk in tokens.chunks(8) {
        let mut control = 0u8;
        let mut bytes = Vec::new();
        for (bit, token) in chunk.iter().enumerate() {
            match token {
                Some(reference) => {
                    control |= 1 << bit;
                    bytes.extend(reference.to_be_bytes());
                }
                None => bytes.push(*literals.next().unwrap()),
            }
        }
        body.push(control);
        body.extend(bytes);
    }
    let mut out = Vec::new();
    out.extend(((body.len() + 12) as u32).to_le_bytes());
    out.extend((raw_size as u32).to_le_bytes());
    out.extend(0x7546_5A4Cu32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(body);
    out
}

/// A message with a body of 2 MiB of paragraphs, in RTF of a quarter of that, and `inner`
/// attached `copies` times.
fn heavy(inner: Option<&[u8]>, copies: usize) -> Vec<u8> {
    let rtf = packed_repeat(b"{\\rtf1 ", b"\\par ", (2 << 20) / 5);
    let mut t = Tnef::new();
    t.message_class("IPM.Note");
    t.message_props(&Props::new().binary(mapi::PR_RTF_COMPRESSED, &rtf));
    if let Some(inner) = inner {
        for _ in 0..copies {
            t.attachment(
                "Weitergeleitet",
                &[],
                &Props::new().long(mapi::PR_ATTACH_METHOD, 5).object(mapi::PR_ATTACH_DATA, &IID_IMESSAGE, inner),
            );
        }
    }
    t.build()
}

/// Bytes a decoded message holds as output, attached messages included.
fn output(message: &Message) -> usize {
    let body = &message.body;
    let own = body.text.as_ref().map_or(0, String::len)
        + body.html.as_ref().map_or(0, String::len)
        + body.rtf.as_ref().map_or(0, Vec::len);
    own + message.attachments.iter().map(|a| a.data.len() + a.embedded.as_deref().map_or(0, output)).sum::<usize>()
}

#[test]
fn nested_messages_share_one_output_budget() {
    let inner = heavy(None, 0);
    let middle = heavy(Some(&inner), 4);
    let outer = heavy(Some(&middle), 4);
    assert!(outer.len() < 8 << 20, "{}", outer.len());
    let rtf = rtf::decompress(&packed_repeat(b"{\\rtf1 ", b"\\par ", (2 << 20) / 5), 4 << 20).unwrap();
    assert_eq!(rtf.len(), 7 + (2 << 20) / 5 * 5, "the packed RTF unpacks as meant");

    let start = Instant::now();
    let message = decode(&outer).unwrap();
    assert!(start.elapsed() < FAST, "{:?}", start.elapsed());
    // Twenty-one bodies of megabytes each; together they get what one stream of this size may
    // make (16 MiB), give or take a few closing tags.
    let made = output(&message);
    assert!(made <= (16 << 20) + (64 << 10), "{made}");
    assert!(message.body.rtf.is_some(), "what fits is kept");
    assert!(!message.complete, "what did not fit is reported");
}
