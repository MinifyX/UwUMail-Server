//! JMAP ids: a type letter plus the database id, and blob ids built from content hashes.

use uwumail_store::BlobHash;

pub fn account(id: i64) -> String {
    format!("a{id}")
}

pub fn mailbox(id: i64) -> String {
    format!("m{id}")
}

pub fn email(id: i64) -> String {
    format!("e{id}")
}

pub fn thread(id: i64) -> String {
    format!("t{id}")
}

pub fn identity(id: i64) -> String {
    format!("i{id}")
}

pub fn submission(id: i64) -> String {
    format!("s{id}")
}

pub fn sender(id: i64) -> String {
    format!("l{id}")
}

pub fn calendar(id: i64) -> String {
    format!("c{id}")
}

pub fn calendar_event(id: i64) -> String {
    format!("v{id}")
}

/// One instance of a recurring event, found by expanding it: the event id and the instance's
/// recurrence id without its separators, like `v12_20261027T090000`.
pub fn event_instance(id: i64, recurrence_id: &str) -> String {
    let compact: String = recurrence_id.chars().filter(|c| *c != '-' && *c != ':').collect();
    format!("v{id}_{compact}")
}

/// The event id and recurrence id (`2026-10-27T09:00:00`) of an instance id.
pub fn parse_event_instance(value: &str) -> Option<(i64, String)> {
    let (event, compact) = value.split_once('_')?;
    let b = compact.as_bytes();
    if b.len() != 15 || b[8] != b'T' || !compact.bytes().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit()) {
        return None;
    }
    let rid = format!(
        "{}-{}-{}T{}:{}:{}",
        &compact[0..4],
        &compact[4..6],
        &compact[6..8],
        &compact[9..11],
        &compact[11..13],
        &compact[13..15]
    );
    Some((parse('v', event)?, rid))
}

pub fn participant(account_id: i64) -> String {
    format!("u{account_id}")
}

/// Parses an id with the given type letter.
pub fn parse(prefix: char, value: &str) -> Option<i64> {
    let rest = value.strip_prefix(prefix)?;
    if rest.is_empty() || rest.len() > 18 || !rest.bytes().all(|b| b.is_ascii_digit()) || rest.starts_with('0') {
        return None;
    }
    rest.parse().ok()
}

pub fn blob(hash: &BlobHash) -> String {
    format!("b{hash}")
}

pub fn part_blob(hash: &BlobHash, part: usize) -> String {
    format!("p{hash}_{part}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobRef {
    /// A whole stored blob (a message or an upload).
    Whole(BlobHash),
    /// One MIME part of a stored message, decoded.
    Part(BlobHash, usize),
}

impl BlobRef {
    pub fn hash(&self) -> &BlobHash {
        match self {
            BlobRef::Whole(hash) | BlobRef::Part(hash, _) => hash,
        }
    }
}

pub fn parse_blob(value: &str) -> Option<BlobRef> {
    if let Some(hash) = value.strip_prefix('b') {
        return BlobHash::parse(hash).ok().map(BlobRef::Whole);
    }
    let rest = value.strip_prefix('p')?;
    let (hash, part) = rest.split_once('_')?;
    Some(BlobRef::Part(BlobHash::parse(hash).ok()?, part.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(parse('m', &mailbox(42)), Some(42));
        assert_eq!(parse('m', "e42"), None);
        assert_eq!(parse('m', "m042"), None);
        assert_eq!(parse('m', "m"), None);
        let hash = BlobHash::of(b"hello");
        assert_eq!(parse_blob(&blob(&hash)), Some(BlobRef::Whole(hash.clone())));
        assert_eq!(parse_blob(&part_blob(&hash, 3)), Some(BlobRef::Part(hash, 3)));
        assert_eq!(parse_blob("bnothex"), None);
        assert_eq!(
            parse_event_instance(&event_instance(7, "2026-10-27T09:00:00")),
            Some((7, "2026-10-27T09:00:00".into()))
        );
        assert_eq!(parse_event_instance("v7_2026102"), None);
        assert_eq!(parse_event_instance("v7_2026-10-27T0"), None);
    }
}
