//! Mailbox names as IMAP shows them: full paths with `/` between the levels and `INBOX` for the
//! inbox, whatever it is called in the apps.

use std::collections::HashMap;

use uwumail_store::{ImapMailbox, MailboxRole};

pub const SEPARATOR: char = '/';

#[derive(Debug, Clone)]
pub struct Named {
    pub mailbox: ImapMailbox,
    pub path: String,
    pub has_children: bool,
}

/// The mailboxes of an account with their paths, parents before children.
pub fn named(mailboxes: Vec<ImapMailbox>) -> Vec<Named> {
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
pub fn matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_pattern(pattern);
    fn go(pattern: &[char], path: &[char]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some(('*', rest)) => (0..=path.len()).any(|i| go(rest, &path[i..])),
            Some(('%', rest)) => {
                (0..=path.len()).take_while(|&i| i == 0 || path[i - 1] != SEPARATOR).any(|i| go(rest, &path[i..]))
            }
            Some((c, rest)) => path.first() == Some(c) && go(rest, &path[1..]),
        }
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    // Patterns like "***...": keep the work bounded.
    pattern.len() <= 255 && go(&pattern, &path)
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
        let named = named(vec![
            mailbox(1, None, "Posteingang", Some(MailboxRole::Inbox)),
            mailbox(2, Some(1), "Rechnungen", None),
            mailbox(3, None, "Projekte", None),
            mailbox(4, Some(3), "UwUMail", None),
            mailbox(5, None, "Sent", Some(MailboxRole::Sent)),
        ]);
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
}
