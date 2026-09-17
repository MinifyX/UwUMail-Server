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
    }
}
