//! What the IMAP parsers do with input nobody sane would send.
//!
//! `parse_command` is the first thing an unauthenticated stranger on port 993 reaches, and
//! `mime::parse` runs over whatever arrived in a mailbox. Neither may panic, hang or eat memory,
//! whatever it is handed — a panic in a parser is a way to take the server down without logging in.
//!
//! This is fuzzing in the small: a fixed seed so a failure can be reproduced from the output, a
//! corpus of real commands, and mutations of them. `cargo fuzz` would go deeper, but it needs
//! nightly and libFuzzer and does not support Windows, and a test that only ever runs somewhere
//! else is a test that stops running. Set `UWUMAIL_FUZZ_ROUNDS` for a longer local run.

use std::time::{Duration, Instant};

use uwumail_imap::mime;
use uwumail_imap::parser::{literal_announcement, parse_command, parse_continuation, parse_date, parse_date_time};

/// Deterministic, so a failing round prints a seed that reproduces it.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*: small, good enough for shaking a parser, and the same everywhere.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, limit: usize) -> usize {
        if limit == 0 { 0 } else { (self.next() % limit as u64) as usize }
    }

    fn byte(&mut self) -> u8 {
        (self.next() & 0xff) as u8
    }
}

/// Commands as real clients send them, including the shapes with the most parsing in them.
const CORPUS: &[&str] = &[
    "a1 CAPABILITY\r\n",
    "a2 LOGIN mini@example.com hunter2\r\n",
    "a3 LOGIN {4+}\r\nmini {6+}\r\nsecret\r\n",
    "a4 AUTHENTICATE PLAIN\r\n",
    "a5 SELECT INBOX\r\n",
    "a6 SELECT \"Sent Items\"\r\n",
    "a7 LIST \"\" *\r\n",
    "a8 STATUS INBOX (MESSAGES UNSEEN UIDNEXT UIDVALIDITY)\r\n",
    "a9 FETCH 1:* (UID FLAGS BODY.PEEK[HEADER.FIELDS (FROM TO SUBJECT)]<0.1024>)\r\n",
    "b1 UID FETCH 1,3,5:9 (BODYSTRUCTURE ENVELOPE INTERNALDATE RFC822.SIZE)\r\n",
    "b2 SEARCH OR FROM \"a@b\" (SINCE 1-Jan-2026 NOT SEEN) TEXT \"hello\"\r\n",
    "b3 UID SEARCH RETURN (MIN MAX COUNT) UNDELETED\r\n",
    "b4 STORE 1:4 +FLAGS.SILENT (\\Seen \\Answered)\r\n",
    "b5 APPEND INBOX (\\Seen) \"1-Jan-2026 10:00:00 +0100\" {12}\r\nHello there!\r\n",
    "b6 COPY 2:4 Archive\r\n",
    "b7 UID MOVE 7 \"Trash\"\r\n",
    "b8 IDLE\r\n",
    "b9 ENABLE UTF8=ACCEPT CONDSTORE\r\n",
    "c1 GETQUOTAROOT INBOX\r\n",
    "c2 CREATE \"Ordner/Unterordner\"\r\n",
    "c3 RENAME &APwA5ADk- Umlaute\r\n",
    "c4 SETMETADATA INBOX (/private/comment \"nyu\")\r\n",
    "c5 LOGOUT\r\n",
];

/// Turns a command into something a little wrong, in one of the ways that break parsers.
fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    let rounds = 1 + rng.below(6);
    for _ in 0..rounds {
        if out.is_empty() {
            out.push(rng.byte());
            continue;
        }
        match rng.below(8) {
            // Flip a byte: the plain one.
            0 => {
                let at = rng.below(out.len());
                out[at] = rng.byte();
            }
            // Cut it short, mid-token or mid-literal.
            1 => out.truncate(rng.below(out.len())),
            // Grow it: parsers that count characters tend to trip over the long ones.
            2 => {
                let at = rng.below(out.len());
                let run = 1 + rng.below(4096);
                let filler = out[at];
                out.splice(at..at, std::iter::repeat_n(filler, run));
            }
            // Tamper with a literal's announced length, which is the number the reader trusts.
            3 => {
                if let Some(open) = out.iter().position(|&b| b == b'{') {
                    let close = out[open..].iter().position(|&b| b == b'}').map(|i| open + i);
                    if let Some(close) = close {
                        let sizes: &[&[u8]] =
                            &[b"0", b"-1", b"+1", b"99999999999999999999", b"4294967295", b"18446744073709551615", b""];
                        let size = sizes[rng.below(sizes.len())];
                        out.splice(open + 1..close, size.iter().copied());
                    }
                }
            }
            // Take the line endings away, or double them.
            4 => {
                out = match rng.below(2) {
                    0 => out.iter().copied().filter(|&b| b != b'\r' && b != b'\n').collect(),
                    _ => out.iter().flat_map(|&b| if b == b'\n' { vec![b, b] } else { vec![b] }).collect(),
                };
            }
            // Splice in another command: two halves that never belonged together.
            5 => {
                let other = CORPUS[rng.below(CORPUS.len())].as_bytes();
                let at = rng.below(out.len());
                out.splice(at..at, other.iter().copied());
            }
            // Bytes no text protocol expects: NUL, high bytes, broken UTF-8.
            6 => {
                let at = rng.below(out.len());
                let nasty: &[u8] = &[0x00, 0x80, 0xff, 0xc0, 0xfe, 0x1b, b'"', b'\\', b'(', b')', b'{', b'}'];
                out.insert(at, nasty[rng.below(nasty.len())]);
            }
            // Nesting, which is where recursive parsers run out of stack.
            _ => {
                let depth = 1 + rng.below(64);
                let mut wrapped = b"z SEARCH ".to_vec();
                wrapped.extend(std::iter::repeat_n(b'(', depth));
                wrapped.extend_from_slice(b"ALL");
                wrapped.extend(std::iter::repeat_n(b')', depth));
                wrapped.extend_from_slice(b"\r\n");
                out = wrapped;
            }
        }
    }
    out
}

fn rounds() -> usize {
    std::env::var("UWUMAIL_FUZZ_ROUNDS").ok().and_then(|value| value.parse().ok()).unwrap_or(20_000)
}

/// Every parser that sees bytes from a stranger, over mutated real commands and pure noise.
///
/// A panic fails the test by itself; the seed in the message is what reproduces it.
#[test]
fn the_parsers_survive_nonsense() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let started = Instant::now();
    let rounds = rounds();
    for round in 0..rounds {
        let input = if round % 4 == 3 {
            // Pure noise as well: mutations keep some structure, and structure is a blind spot.
            (0..rng.below(2048)).map(|_| rng.byte()).collect::<Vec<u8>>()
        } else {
            let seed = CORPUS[rng.below(CORPUS.len())].as_bytes();
            mutate(&mut rng, seed)
        };

        let what = format!("round {round} of seed 0x5eed1234abcd0001, {} bytes", input.len());
        let one = Instant::now();

        // The two that a stranger reaches before logging in.
        let _ = parse_command(&input, false);
        let _ = parse_command(&input, true);
        let _ = literal_announcement(&input);
        let _ = parse_continuation(&input);
        // And the ones that read what arrived in a mailbox.
        let _ = mime::parse(&input);
        if let Ok(text) = std::str::from_utf8(&input) {
            let _ = parse_date(text);
            let _ = parse_date_time(text);
        }

        // Not a benchmark: a second on a few kilobytes means something is quadratic, and that is
        // as good as a crash when anyone can send it.
        assert!(one.elapsed() < Duration::from_secs(1), "a parser took {:?} on {what}", one.elapsed());
    }
    println!("{rounds} rounds in {:?}", started.elapsed());
}

/// The lengths a literal may claim, which is the one number the reader allocates from.
#[test]
fn an_announced_length_is_never_believed_blindly() {
    // Nothing here may panic, and nothing absurd may come back as a usable size.
    for line in [
        "a APPEND INBOX {18446744073709551615}\r\n",
        "a APPEND INBOX {99999999999999999999}\r\n",
        "a APPEND INBOX {-1}\r\n",
        "a APPEND INBOX {+5}\r\n",
        "a APPEND INBOX { 5 }\r\n",
        "a APPEND INBOX {5\r\n",
        "a APPEND INBOX {}\r\n",
        "a APPEND INBOX {0}\r\n",
        "a APPEND INBOX {00000000005}\r\n",
    ] {
        let announced = literal_announcement(line.as_bytes());
        assert!(
            announced.is_none_or(|(size, _)| size <= 9_999_999_999),
            "{line:?} announced {announced:?}, which is more than ten digits could say"
        );
    }
    assert_eq!(literal_announcement(b"a APPEND INBOX {0}\r\n"), Some((0, false)));
    assert_eq!(literal_announcement(b"a LOGIN {4+}\r\n"), Some((4, true)));
}
