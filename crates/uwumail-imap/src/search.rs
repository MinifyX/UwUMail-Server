//! SEARCH: criteria checked against a mailbox's messages. Text in the body goes through the
//! store's full-text index; header fields other than the usual ones are read from the message.

use std::collections::{HashMap, HashSet};

use uwumail_store::{EmailAddress, ImapEmail, Store};

use crate::command::SearchKey;
use crate::mime;

/// What the criteria need besides the message rows, gathered once per SEARCH.
#[derive(Default)]
pub struct Prepared {
    /// Email ids per full-text term.
    text: HashMap<String, HashSet<i64>>,
    /// Header bytes per email id, only when a HEADER key asks for them.
    headers: HashMap<i64, Vec<u8>>,
}

fn walk<'a>(key: &'a SearchKey, visit: &mut impl FnMut(&'a SearchKey)) {
    visit(key);
    match key {
        SearchKey::And(keys) => keys.iter().for_each(|key| walk(key, visit)),
        SearchKey::Or(left, right) => {
            walk(left, visit);
            walk(right, visit);
        }
        SearchKey::Not(inner) => walk(inner, visit),
        _ => {}
    }
}

/// Looks up full-text terms and, if needed, message headers.
pub async fn prepare(
    store: &Store,
    account_id: i64,
    key: &SearchKey,
    emails: &[ImapEmail],
) -> uwumail_store::Result<Prepared> {
    let mut terms = Vec::new();
    let mut wants_headers = false;
    walk(key, &mut |key| match key {
        SearchKey::Body(term) | SearchKey::Text(term) => terms.push(term.clone()),
        SearchKey::Header(..) => wants_headers = true,
        _ => {}
    });
    let mut prepared = Prepared::default();
    for term in terms {
        if prepared.text.contains_key(&term) {
            continue;
        }
        let ids = store.search_emails(account_id, &term).await?.into_iter().collect();
        prepared.text.insert(term, ids);
    }
    if wants_headers {
        for email in emails {
            let raw = store.blob(&email.blob).await?;
            let root = mime::parse(&raw);
            prepared.headers.insert(email.email_id, raw[root.header].to_vec());
        }
    }
    Ok(prepared)
}

fn contains(haystack: &str, needle: &str) -> bool {
    needle.is_empty() || haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn addresses_contain(list: &[EmailAddress], needle: &str) -> bool {
    list.iter().any(|address| {
        contains(&address.email, needle) || address.name.as_deref().is_some_and(|name| contains(name, needle))
    })
}

fn day(unix: i64) -> i64 {
    unix.div_euclid(86_400)
}

pub struct Target<'a> {
    pub msn: u32,
    pub email: &'a ImapEmail,
}

pub struct Scope {
    pub largest_msn: u32,
    pub largest_uid: u32,
    pub now: i64,
}

pub fn matches(key: &SearchKey, target: &Target<'_>, scope: &Scope, prepared: &Prepared) -> bool {
    let email = target.email;
    let has = |keyword: &str| email.keywords.iter().any(|k| k == keyword);
    match key {
        SearchKey::All => true,
        SearchKey::And(keys) => keys.iter().all(|key| matches(key, target, scope, prepared)),
        SearchKey::Or(left, right) => matches(left, target, scope, prepared) || matches(right, target, scope, prepared),
        SearchKey::Not(inner) => !matches(inner, target, scope, prepared),
        SearchKey::SequenceSet(set) => set.contains(target.msn, scope.largest_msn),
        SearchKey::Uid(set) => set.contains(email.uid, scope.largest_uid),
        SearchKey::Keyword(keyword) => has(keyword),
        SearchKey::Unkeyword(keyword) => !has(keyword),
        // No message is recent: this server does not keep \Recent.
        SearchKey::New | SearchKey::Recent => false,
        SearchKey::Old => true,
        SearchKey::Bcc(text) => addresses_contain(&email.bcc, text),
        SearchKey::Cc(text) => addresses_contain(&email.cc, text),
        SearchKey::From(text) => addresses_contain(&email.from, text),
        SearchKey::To(text) => addresses_contain(&email.to, text),
        SearchKey::Subject(text) => contains(&email.subject, text),
        SearchKey::Body(text) | SearchKey::Text(text) => {
            text.trim().is_empty() || prepared.text.get(text).is_some_and(|ids| ids.contains(&email.email_id))
        }
        SearchKey::Header(field, text) => prepared.headers.get(&email.email_id).is_some_and(|header| {
            let range = 0..header.len();
            mime::fields(header, &range).iter().any(|f| f.name.eq_ignore_ascii_case(field) && contains(&f.value, text))
        }),
        SearchKey::Before(d) => day(email.received_at) < *d,
        SearchKey::On(d) => day(email.received_at) == *d,
        SearchKey::Since(d) => day(email.received_at) >= *d,
        SearchKey::SentBefore(d) => day(email.sent_at.unwrap_or(email.received_at)) < *d,
        SearchKey::SentOn(d) => day(email.sent_at.unwrap_or(email.received_at)) == *d,
        SearchKey::SentSince(d) => day(email.sent_at.unwrap_or(email.received_at)) >= *d,
        SearchKey::Larger(size) => email.size > *size,
        SearchKey::Smaller(size) => email.size < *size,
        SearchKey::ModSeq(modseq) => email.modseq >= *modseq,
        SearchKey::Younger(seconds) => scope.now - email.received_at <= *seconds as i64,
        SearchKey::Older(seconds) => scope.now - email.received_at > *seconds as i64,
    }
}

/// Whether the criteria ask about modseqs, which makes SEARCH answer with the highest one.
pub fn uses_modseq(key: &SearchKey) -> bool {
    let mut found = false;
    walk(key, &mut |key| found |= matches!(key, SearchKey::ModSeq(_)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{SeqNum, SequenceSet};
    use uwumail_store::BlobHash;

    fn email(uid: u32, subject: &str, from: &str, keywords: &[&str], received_at: i64) -> ImapEmail {
        ImapEmail {
            uid,
            email_id: uid as i64 + 100,
            modseq: uid as u64,
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            blob: BlobHash::of(b"x"),
            size: 1000 * uid as u64,
            received_at,
            sent_at: None,
            subject: subject.into(),
            from: vec![EmailAddress {
                name: Some(format!("{} Katze", from.split('@').next().unwrap_or_default())),
                email: from.into(),
            }],
            to: vec![],
            cc: vec![],
            bcc: vec![],
        }
    }

    #[test]
    fn criteria_combine() {
        let day = 86_400 * 20_000;
        let emails = [
            email(1, "Katzenfutter", "nyu@example.org", &["$seen"], day),
            email(2, "Rechnung", "shop@example.com", &[], day + 86_400),
            email(3, "Re: Katzenfutter", "leni@example.de", &["$flagged"], day + 2 * 86_400),
        ];
        let scope = Scope { largest_msn: 3, largest_uid: 3, now: day + 2 * 86_400 + 60 };
        let prepared = Prepared::default();
        let found = |key: SearchKey| -> Vec<u32> {
            emails
                .iter()
                .enumerate()
                .filter(|(i, email)| matches(&key, &Target { msn: *i as u32 + 1, email }, &scope, &prepared))
                .map(|(_, email)| email.uid)
                .collect()
        };
        assert_eq!(found(SearchKey::Subject("katzen".into())), vec![1, 3]);
        assert_eq!(found(SearchKey::From("NYU KATZE".into())), vec![1]);
        assert_eq!(found(SearchKey::Unkeyword("$seen".into())), vec![2, 3]);
        assert_eq!(
            found(SearchKey::Or(Box::new(SearchKey::Keyword("$flagged".into())), Box::new(SearchKey::Larger(1500)))),
            vec![2, 3]
        );
        assert_eq!(found(SearchKey::Since(20_001)), vec![2, 3]);
        assert_eq!(found(SearchKey::On(20_000)), vec![1]);
        assert_eq!(found(SearchKey::Younger(3600)), vec![3]);
        assert_eq!(found(SearchKey::SequenceSet(SequenceSet(vec![(SeqNum::Value(2), SeqNum::Largest)]))), vec![2, 3]);
        assert_eq!(
            found(SearchKey::Not(Box::new(SearchKey::Uid(SequenceSet(vec![(SeqNum::Largest, SeqNum::Largest)]))))),
            vec![1, 2]
        );
        assert!(uses_modseq(&SearchKey::And(vec![SearchKey::All, SearchKey::ModSeq(2)])));
    }
}
