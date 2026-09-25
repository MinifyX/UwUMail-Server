//! Mailbox names as IMAP shows them: full paths with `/` between the levels and `INBOX` for the
//! inbox, whatever it is called in the apps.

use std::collections::{BTreeMap, HashMap};

use uwumail_store::{ALL_RIGHTS, ImapMailbox, MailboxRole, SharedMailbox};

pub const SEPARATOR: char = '/';

/// Where other people's mailboxes shared with the account show: `Shared/<their login>/<path>`
/// (the "Other Users" namespace of RFC 2342).
pub const SHARED_PREFIX: &str = "Shared";

#[derive(Debug, Clone)]
pub struct Named {
    pub mailbox: ImapMailbox,
    pub path: String,
    pub has_children: bool,
    /// The account the mailbox belongs to.
    pub owner: i64,
    /// What the logged-in account may do with it (RFC 4314 letters): everything with its own.
    pub rights: String,
    /// Only a level of the shared namespace (`Shared`, `Shared/<person>`, or a folder of someone
    /// else that is not shared itself but holds one that is): listed, never selected.
    pub placeholder: bool,
}

impl Named {
    pub fn may(&self, rights: &str) -> bool {
        !self.placeholder && uwumail_store::has_rights(&self.rights, rights)
    }
}

/// The mailboxes of `owner` with their paths, parents before children.
pub fn named(mailboxes: Vec<ImapMailbox>, owner: i64) -> Vec<Named> {
    let by_id: HashMap<i64, &ImapMailbox> = mailboxes.iter().map(|mailbox| (mailbox.id, mailbox)).collect();
    let path_of = |mailbox: &ImapMailbox| {
        let mut names = Vec::new();
        let mut current = Some(mailbox);
        let mut depth = 0;
        while let Some(mailbox) = current {
            if mailbox.role == Some(MailboxRole::Inbox) && mailbox.parent_id.is_none() {
                names.push("INBOX".to_owned());
                break;
            }
            names.push(mailbox.name.clone());
            current = mailbox.parent_id.and_then(|parent| by_id.get(&parent).copied());
            depth += 1;
            if depth > 64 {
                break;
            }
        }
        names.reverse();
        names.join(&SEPARATOR.to_string())
    };
    let mut named: Vec<Named> = mailboxes
        .iter()
        .map(|mailbox| Named {
            path: path_of(mailbox),
            has_children: mailboxes.iter().any(|other| other.parent_id == Some(mailbox.id)),
            mailbox: mailbox.clone(),
            owner,
            rights: ALL_RIGHTS.to_owned(),
            placeholder: false,
        })
        .collect();
    // INBOX first, then the other special folders in their order, then alphabetically by path.
    named.sort_by(|a, b| {
        let rank = |n: &Named| match n.mailbox.role {
            Some(MailboxRole::Inbox) if n.mailbox.parent_id.is_none() => 0,
            _ => 1,
        };
        rank(a).cmp(&rank(b)).then_with(|| a.path.cmp(&b.path))
    });
    named
}

/// Other people's mailboxes shared with the account, under `Shared/<their login>/`, with the
/// levels above them as placeholders. `trees` holds every mailbox of each owner, for the paths:
/// a shared folder keeps the path it has for its owner. Sorted by path.
pub fn shared_named(shared: Vec<SharedMailbox>, trees: &HashMap<i64, Vec<ImapMailbox>>) -> Vec<Named> {
    let mut by_path: BTreeMap<String, Named> = BTreeMap::new();
    if shared.is_empty() {
        return Vec::new();
    }
    let placeholder = |path: &str, owner: i64| Named {
        mailbox: ImapMailbox {
            id: 0,
            parent_id: None,
            name: path.rsplit(SEPARATOR).next().unwrap_or(path).to_owned(),
            role: None,
            subscribed: true,
            uid_validity: 1,
            uid_next: 1,
        },
        path: path.to_owned(),
        has_children: true,
        owner,
        rights: String::new(),
        placeholder: true,
    };
    by_path.insert(SHARED_PREFIX.to_owned(), placeholder(SHARED_PREFIX, 0));
    let mut paths_of: HashMap<i64, HashMap<i64, String>> = HashMap::new();
    for entry in shared {
        let paths = paths_of.entry(entry.owner_id).or_insert_with(|| {
            let tree = trees.get(&entry.owner_id).cloned().unwrap_or_default();
            named(tree, entry.owner_id).into_iter().map(|n| (n.mailbox.id, n.path)).collect()
        });
        let Some(own_path) = paths.get(&entry.mailbox.id) else { continue };
        let prefix = format!("{SHARED_PREFIX}{SEPARATOR}{}", entry.owner_login);
        by_path.entry(prefix.clone()).or_insert_with(|| placeholder(&prefix, entry.owner_id));
        let levels: Vec<&str> = own_path.split(SEPARATOR).collect();
        for depth in 1..levels.len() {
            let ancestor = format!("{prefix}{SEPARATOR}{}", levels[..depth].join(&SEPARATOR.to_string()));
            by_path.entry(ancestor.clone()).or_insert_with(|| placeholder(&ancestor, entry.owner_id));
        }
        let path = format!("{prefix}{SEPARATOR}{own_path}");
        // Someone else's special folders are not this account's: no special use for them.
        let mailbox = ImapMailbox { role: None, subscribed: true, ..entry.mailbox };
        by_path.insert(
            path.clone(),
            Named {
                mailbox,
                path,
                has_children: false,
                owner: entry.owner_id,
                rights: entry.rights,
                placeholder: false,
            },
        );
    }
    let paths: Vec<String> = by_path.keys().cloned().collect();
    for (path, named) in by_path.iter_mut() {
        let inside = format!("{path}{SEPARATOR}");
        named.has_children = paths.iter().any(|other| other.starts_with(&inside));
    }
    by_path.into_values().collect()
}

/// The part of a shared path after `Shared/<login>/`, and the login.
pub fn split_shared(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(SHARED_PREFIX)?.strip_prefix(SEPARATOR)?;
    let (login, inner) = rest.split_once(SEPARATOR)?;
    Some((login, inner))
}

/// Finds a mailbox by its IMAP path. `INBOX` is case-insensitive, like in every client.
pub fn find<'a>(named: &'a [Named], path: &str) -> Option<&'a Named> {
    let path = normalize(path);
    named.iter().find(|n| n.path == path)
}

/// `inbox/Foo/` → `INBOX/Foo`.
pub fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches(SEPARATOR);
    match trimmed.split_once(SEPARATOR) {
        Some((first, rest)) if first.eq_ignore_ascii_case("INBOX") => format!("INBOX{SEPARATOR}{rest}"),
        None if trimmed.eq_ignore_ascii_case("INBOX") => "INBOX".to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// Whether a path matches a LIST pattern: `*` matches anything, `%` anything but the separator.
///
/// Walks the path once, keeping the set of pattern positions reached so far, so the work is at most
/// pattern length times path length. Trying every split for every wildcard, as this did before, took
/// exponential time on a pattern like `*a*a*a…b` against a long name of `a`s: one LIST from a
/// logged-in account held a worker thread for good (security-audit-0.8.0 A-1).
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = normalize_pattern(pattern).chars().collect();
    if pattern.len() > 255 {
        return false;
    }
    // `reached[i]`: the path read so far matches the first `i` characters of the pattern.
    let mut reached = vec![false; pattern.len() + 1];
    reached[0] = true;
    let close = |reached: &mut [bool]| {
        // A wildcard may also match nothing.
        for i in 0..pattern.len() {
            if reached[i] && matches!(pattern[i], '*' | '%') {
                reached[i + 1] = true;
            }
        }
    };
    close(&mut reached);
    let mut next = vec![false; pattern.len() + 1];
    for c in path.chars() {
        next.fill(false);
        for (i, &p) in pattern.iter().enumerate() {
            if !reached[i] {
                continue;
            }
            match p {
                '*' => next[i] = true,
                '%' if c != SEPARATOR => next[i] = true,
                '%' => {}
                literal if literal == c => next[i + 1] = true,
                _ => {}
            }
        }
        close(&mut next);
        if !next.contains(&true) {
            return false;
        }
        std::mem::swap(&mut reached, &mut next);
    }
    reached[pattern.len()]
}

fn normalize_pattern(pattern: &str) -> String {
    match pattern.get(..5) {
        Some(first)
            if first.eq_ignore_ascii_case("INBOX") && pattern[5..].chars().next().is_none_or(|c| c == SEPARATOR) =>
        {
            format!("INBOX{}", &pattern[5..])
        }
        _ => pattern.to_owned(),
    }
}

pub fn special_use(role: Option<MailboxRole>) -> Option<&'static str> {
    match role? {
        MailboxRole::Inbox => None,
        MailboxRole::Drafts => Some("\\Drafts"),
        MailboxRole::Sent => Some("\\Sent"),
        MailboxRole::Archive => Some("\\Archive"),
        MailboxRole::Junk => Some("\\Junk"),
        MailboxRole::Trash => Some("\\Trash"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(id: i64, parent_id: Option<i64>, name: &str, role: Option<MailboxRole>) -> ImapMailbox {
        ImapMailbox { id, parent_id, name: name.into(), role, subscribed: true, uid_validity: 1, uid_next: 1 }
    }

    #[test]
    fn paths_and_lookup() {
        let named = named(
            vec![
                mailbox(1, None, "Posteingang", Some(MailboxRole::Inbox)),
                mailbox(2, Some(1), "Rechnungen", None),
                mailbox(3, None, "Projekte", None),
                mailbox(4, Some(3), "UwUMail", None),
                mailbox(5, None, "Sent", Some(MailboxRole::Sent)),
            ],
            1,
        );
        let paths: Vec<&str> = named.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(paths, vec!["INBOX", "INBOX/Rechnungen", "Projekte", "Projekte/UwUMail", "Sent"]);
        assert!(named[0].has_children && !named[1].has_children);
        assert_eq!(find(&named, "inbox/Rechnungen/").unwrap().mailbox.id, 2);
        assert!(find(&named, "projekte").is_none(), "only INBOX ignores case");
    }

    #[test]
    fn patterns() {
        assert!(matches("*", "Projekte/UwUMail"));
        assert!(!matches("%", "Projekte/UwUMail"));
        assert!(matches("Projekte/%", "Projekte/UwUMail"));
        assert!(matches("inbox", "INBOX"));
        assert!(matches("Inbox/*", "INBOX/Rechnungen"));
        assert!(!matches("Inboxen", "INBOX"));
        assert!(matches("P%e", "Projekte"));
        assert!(!matches("*".repeat(300).as_str(), "x"));
    }

    /// The matcher as it was: every split for every wildcard. Right, but exponential.
    fn by_trying_every_split(pattern: &[char], path: &[char]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some(('*', rest)) => (0..=path.len()).any(|i| by_trying_every_split(rest, &path[i..])),
            Some(('%', rest)) => (0..=path.len())
                .take_while(|&i| i == 0 || path[i - 1] != SEPARATOR)
                .any(|i| by_trying_every_split(rest, &path[i..])),
            Some((c, rest)) => path.first() == Some(c) && by_trying_every_split(rest, &path[1..]),
        }
    }

    /// Every combination up to four characters, against the old matcher.
    #[test]
    fn patterns_match_as_before() {
        fn all(alphabet: &[char], max: usize) -> Vec<String> {
            let mut out = vec![String::new()];
            let mut last = vec![String::new()];
            for _ in 0..max {
                last = last.iter().flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}"))).collect();
                out.extend(last.iter().cloned());
            }
            out
        }
        let patterns = all(&['a', 'b', '/', '*', '%'], 4);
        let paths = all(&['a', 'b', '/'], 4);
        for pattern in &patterns {
            let chars: Vec<char> = pattern.chars().collect();
            for path in &paths {
                let path_chars: Vec<char> = path.chars().collect();
                assert_eq!(
                    matches(pattern, path),
                    by_trying_every_split(&chars, &path_chars),
                    "{pattern:?} against {path:?}"
                );
            }
        }
    }

    /// security-audit-0.8.0 A-1: the longest pattern a client may send, built to make the old matcher
    /// backtrack, against a long name that almost matches. Answered at once.
    #[test]
    fn a_hostile_pattern_is_answered_at_once() {
        let started = std::time::Instant::now();
        let pattern = format!("{}b", "*a".repeat(127));
        assert_eq!(pattern.len(), 255);
        let name = "a".repeat(255);
        assert!(!matches(&pattern, &name));
        assert!(!matches(&"%a".repeat(127), &format!("{name}b")));
        assert!(matches(&"*a".repeat(127), &name));
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
    }
}
