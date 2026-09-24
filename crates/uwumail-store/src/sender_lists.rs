//! Senders that are always let through or always kept out, for one person, one of our domains or
//! the whole server. The SMTP side decides what a match means; this keeps the entries tidy.

use std::net::IpAddr;

use rusqlite::{Row, ToSql, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::address::{normalize_address, normalize_domain};
use crate::{Result, Store, StoreError, now};

/// How many entries one person may keep.
pub const SENDER_LIST_PERSONAL_LIMIT: i64 = 1000;
/// How many entries the whole server and each domain may keep.
pub const SENDER_LIST_ADMIN_LIMIT: i64 = 10_000;
const NOTE_MAX_CHARS: usize = 200;
/// Networks wider than this would cover whole providers or continents.
const MIN_PREFIX_V4: u8 = 8;
const MIN_PREFIX_V6: u8 = 16;
/// A pattern needs this much besides `*`, or it would match nearly everyone.
const PATTERN_MIN_TEXT: usize = 3;
const PATTERN_MAX_CHARS: usize = 254;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SenderList {
    Allow,
    Block,
}

impl SenderList {
    pub fn as_str(self) -> &'static str {
        match self {
            SenderList::Allow => "allow",
            SenderList::Block => "block",
        }
    }
}

/// What an entry names: the sending server by address or confirmed host name, or the sender by
/// address, domain or a pattern with `*` over the whole address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SenderKind {
    Ip,
    Host,
    Address,
    Domain,
    Pattern,
}

impl SenderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SenderKind::Ip => "ip",
            SenderKind::Host => "host",
            SenderKind::Address => "address",
            SenderKind::Domain => "domain",
            SenderKind::Pattern => "pattern",
        }
    }
}

/// Whose list an entry is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListScope {
    Server,
    Domain(i64),
    Account(i64),
}

impl ListScope {
    pub(crate) fn ids(self) -> (Option<i64>, Option<i64>) {
        match self {
            ListScope::Server => (None, None),
            ListScope::Domain(id) => (None, Some(id)),
            ListScope::Account(id) => (Some(id), None),
        }
    }

    pub(crate) fn limit(self) -> i64 {
        match self {
            ListScope::Account(_) => SENDER_LIST_PERSONAL_LIMIT,
            _ => SENDER_LIST_ADMIN_LIMIT,
        }
    }
}

/// Who changes an entry: admins look after every list, people after their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListOwner {
    Admin,
    Account(i64),
}

impl ListOwner {
    pub fn looks_after(self, scope: ListScope) -> bool {
        match (self, scope) {
            (ListOwner::Admin, _) => true,
            (ListOwner::Account(owner), ListScope::Account(account)) => owner == account,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderListEntry {
    pub id: i64,
    pub list: SenderList,
    pub kind: SenderKind,
    pub value: String,
    pub note: String,
    /// The domain of a domain-wide entry.
    pub domain: Option<String>,
    #[serde(skip)]
    pub scope: ListScope,
    pub created_at: i64,
    pub created_by: String,
    /// When the entry runs out, in Unix seconds; `None` for good.
    pub expires_at: Option<i64>,
    /// How often it decided for a message, and when last.
    pub hits: i64,
    pub last_hit_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewSenderListEntry {
    pub scope: ListScope,
    pub list: SenderList,
    /// Guessed from the value when left out.
    pub kind: Option<SenderKind>,
    pub value: String,
    pub note: String,
    pub created_by: String,
    /// When the entry runs out, in Unix seconds; `None` for good.
    pub expires_at: Option<i64>,
}

fn invalid(message: String) -> StoreError {
    StoreError::Rule { code: "senderInvalid", message }
}

fn parse_network(value: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = match value.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (value, None),
    };
    let address = address.parse::<IpAddr>().ok()?.to_canonical();
    let max = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix.parse::<u8>().ok().filter(|prefix| *prefix <= max)?,
        None => max,
    };
    Some((address, prefix))
}

/// What a value most likely is: an address or network, a `*.` host name like `*.mail.example.com`,
/// any other value with `*` a pattern, a full email address, otherwise a domain. A single host name
/// needs to be asked for as a host.
pub fn guess_sender_kind(value: &str) -> SenderKind {
    let value = value.trim();
    if parse_network(value).is_some() {
        SenderKind::Ip
    } else if value.strip_prefix("*.").is_some_and(|rest| rest.contains('.') && !rest.contains(['*', '@'])) {
        SenderKind::Host
    } else if value.contains('*') {
        SenderKind::Pattern
    } else if value.contains('@') && !value.starts_with('@') {
        SenderKind::Address
    } else {
        SenderKind::Domain
    }
}

/// A lowercase pattern where `*` stands for any text, runs of `*` squeezed into one.
fn normalize_pattern(original: &str) -> Result<String> {
    if !original.contains('*') {
        return Err(invalid(format!("'{original}' has no '*'; list it as an address or domain")));
    }
    if original.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(format!("'{original}' may not contain spaces")));
    }
    let mut pattern = String::with_capacity(original.len());
    for c in original.chars().flat_map(char::to_lowercase) {
        if !(c == '*' && pattern.ends_with('*')) {
            pattern.push(c);
        }
    }
    if pattern.chars().filter(|c| *c != '*').count() < PATTERN_MIN_TEXT {
        return Err(invalid(format!("'{original}' would match nearly every sender")));
    }
    if pattern.chars().count() > PATTERN_MAX_CHARS {
        return Err(invalid(format!("'{original}' is too long")));
    }
    Ok(pattern)
}

/// Whether `text` fits a normalized pattern, `*` standing for any text including none.
pub fn pattern_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    // Without a '*' the pattern is the text itself.
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// A name with at least two labels; a bare top-level domain would match far too much.
fn normalize_name(name: &str, original: &str) -> Result<String> {
    let name = normalize_domain(name).map_err(|_| invalid(format!("'{original}' is not a valid name")))?;
    if !name.contains('.') {
        return Err(invalid(format!("'{original}' needs at least one dot")));
    }
    Ok(name)
}

/// Brings a value into the one form it is stored and compared in.
pub fn normalize_sender(kind: SenderKind, value: &str) -> Result<String> {
    let original = value.trim();
    match kind {
        SenderKind::Ip => {
            let (address, prefix) = parse_network(original)
                .ok_or_else(|| invalid(format!("'{original}' is not an IP address or network")))?;
            let masked = match address {
                IpAddr::V4(ip) => {
                    if prefix < MIN_PREFIX_V4 {
                        return Err(invalid(format!("'{original}' is wider than /{MIN_PREFIX_V4}")));
                    }
                    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
                    (IpAddr::V4((u32::from(ip) & mask).into()), prefix == 32)
                }
                IpAddr::V6(ip) => {
                    if prefix < MIN_PREFIX_V6 {
                        return Err(invalid(format!("'{original}' is wider than /{MIN_PREFIX_V6}")));
                    }
                    let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
                    (IpAddr::V6((u128::from(ip) & mask).into()), prefix == 128)
                }
            };
            Ok(match masked {
                (address, true) => address.to_string(),
                (address, false) => format!("{address}/{prefix}"),
            })
        }
        SenderKind::Host => match original.strip_prefix("*.") {
            Some(rest) => Ok(format!("*.{}", normalize_name(rest, original)?)),
            None => normalize_name(original, original),
        },
        SenderKind::Address => {
            let (local, domain) = normalize_address(original)
                .map_err(|_| invalid(format!("'{original}' is not a valid email address")))?;
            Ok(format!("{local}@{domain}"))
        }
        SenderKind::Domain => {
            let name = original.trim_start_matches('@');
            let name = name.strip_prefix("*.").unwrap_or(name);
            normalize_name(name, original)
        }
        SenderKind::Pattern => normalize_pattern(original),
    }
}

/// The kind, value and note of a new entry, checked and normalized.
pub(crate) fn checked(new: &NewSenderListEntry) -> Result<(SenderKind, String, String)> {
    let kind = new.kind.unwrap_or_else(|| guess_sender_kind(&new.value));
    let value = normalize_sender(kind, &new.value)?;
    let note = new.note.trim().to_owned();
    if note.chars().count() > NOTE_MAX_CHARS {
        return Err(invalid(format!("the note is longer than {NOTE_MAX_CHARS} characters")));
    }
    Ok((kind, value, note))
}

/// Refuses a value already listed in the scope (on either list), leaving out the entry `except`.
pub(crate) fn check_free(
    tx: &rusqlite::Connection,
    scope: ListScope,
    kind: SenderKind,
    value: &str,
    except: Option<i64>,
) -> Result<()> {
    let (filter, mut values) = scope_filter(scope);
    values.push(Box::new(kind.as_str()));
    values.push(Box::new(value.to_owned()));
    values.push(Box::new(except.unwrap_or(0)));
    if let Some(existing) =
        entries(tx, &format!("{filter} AND l.kind = ? AND l.value = ? AND l.id != ?"), values)?.pop()
    {
        let message = format!("{} is already on the {} list", existing.value, existing.list.as_str());
        return Err(StoreError::Rule { code: "senderListed", message });
    }
    Ok(())
}

/// Refuses one more entry in a scope that is full.
pub(crate) fn check_room(tx: &rusqlite::Connection, scope: ListScope) -> Result<()> {
    let (filter, values) = scope_filter(scope);
    let sql = format!("SELECT COUNT(*) FROM sender_lists l WHERE {filter}");
    let count: i64 = tx.query_row(&sql, params_from_iter(values), |row| row.get(0))?;
    if count >= scope.limit() {
        let message = format!("the list already holds {count} entries");
        return Err(StoreError::Rule { code: "senderListFull", message });
    }
    Ok(())
}

/// Inserts a checked entry and returns its id.
pub(crate) fn insert(
    tx: &Transaction<'_>,
    new: &NewSenderListEntry,
    kind: SenderKind,
    value: &str,
    note: &str,
) -> Result<i64> {
    check_free(tx, new.scope, kind, value, None)?;
    check_room(tx, new.scope)?;
    let (account_id, domain_id) = new.scope.ids();
    tx.execute(
        "INSERT INTO sender_lists (account_id, domain_id, list, kind, value, note, created_at, created_by, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            account_id,
            domain_id,
            new.list.as_str(),
            kind.as_str(),
            value,
            note,
            now(),
            new.created_by,
            new.expires_at
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

const SELECT: &str = "SELECT l.id, l.list, l.kind, l.value, l.note, d.name, l.account_id, l.domain_id, l.created_at,
        l.created_by, l.expires_at, l.hits, l.last_hit_at
     FROM sender_lists l LEFT JOIN domains d ON d.id = l.domain_id";

/// Entries that have not run out yet.
const CURRENT: &str = "(l.expires_at IS NULL OR l.expires_at > strftime('%s', 'now'))";

pub(crate) fn kind_from(kind: &str) -> SenderKind {
    match kind {
        "ip" => SenderKind::Ip,
        "host" => SenderKind::Host,
        "address" => SenderKind::Address,
        "pattern" => SenderKind::Pattern,
        _ => SenderKind::Domain,
    }
}

fn entry(row: &Row<'_>) -> rusqlite::Result<SenderListEntry> {
    let list: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let account_id: Option<i64> = row.get(6)?;
    let domain_id: Option<i64> = row.get(7)?;
    Ok(SenderListEntry {
        id: row.get(0)?,
        list: if list == "allow" { SenderList::Allow } else { SenderList::Block },
        kind: kind_from(&kind),
        value: row.get(3)?,
        note: row.get(4)?,
        domain: row.get(5)?,
        scope: match (account_id, domain_id) {
            (Some(id), _) => ListScope::Account(id),
            (None, Some(id)) => ListScope::Domain(id),
            (None, None) => ListScope::Server,
        },
        created_at: row.get(8)?,
        created_by: row.get(9)?,
        expires_at: row.get(10)?,
        hits: row.get(11)?,
        last_hit_at: row.get(12)?,
    })
}

fn entries(tx: &rusqlite::Connection, filter: &str, values: Vec<Box<dyn ToSql>>) -> Result<Vec<SenderListEntry>> {
    let sql = format!("{SELECT} WHERE {filter} ORDER BY l.list, l.kind, l.value");
    let mut stmt = tx.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), entry)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn scope_filter(scope: ListScope) -> (&'static str, Vec<Box<dyn ToSql>>) {
    match scope {
        ListScope::Server => ("l.account_id IS NULL AND l.domain_id IS NULL", vec![]),
        ListScope::Domain(id) => ("l.domain_id = ?", vec![Box::new(id)]),
        ListScope::Account(id) => ("l.account_id = ?", vec![Box::new(id)]),
    }
}

pub(crate) fn find(tx: &rusqlite::Connection, id: i64) -> Result<Option<SenderListEntry>> {
    Ok(entries(tx, "l.id = ?", vec![Box::new(id)])?.pop())
}

impl Store {
    /// One scope's entries, allowed before blocked.
    pub async fn sender_list(&self, scope: ListScope) -> Result<Vec<SenderListEntry>> {
        self.read(move |conn| {
            let (filter, values) = scope_filter(scope);
            entries(conn, filter, values)
        })
        .await
    }

    /// Everything admins look after: the whole server's entries and those of every domain.
    pub async fn admin_sender_lists(&self) -> Result<Vec<SenderListEntry>> {
        self.read(|conn| entries(conn, "l.account_id IS NULL", vec![])).await
    }

    /// The entries that can matter for mail to these domains and people, including the server's.
    pub async fn sender_lists_for(&self, domains: Vec<String>, accounts: Vec<i64>) -> Result<Vec<SenderListEntry>> {
        self.read(move |conn| {
            let marks = |count: usize| vec!["?"; count].join(", ");
            let filter = format!(
                "{CURRENT} AND ((l.account_id IS NULL AND l.domain_id IS NULL) OR d.name IN ({}) OR l.account_id IN ({}))",
                marks(domains.len()),
                marks(accounts.len())
            );
            let mut values: Vec<Box<dyn ToSql>> = Vec::new();
            values.extend(domains.into_iter().map(|domain| Box::new(domain) as Box<dyn ToSql>));
            values.extend(accounts.into_iter().map(|account| Box::new(account) as Box<dyn ToSql>));
            entries(conn, &filter, values)
        })
        .await
    }

    /// Adds an entry. A value can only be on one of the two lists of a scope at a time.
    pub async fn add_sender_list_entry(&self, new: NewSenderListEntry) -> Result<SenderListEntry> {
        let (kind, value, note) = checked(&new)?;
        self.write(move |tx| {
            let id = insert(tx, &new, kind, &value, &note)?;
            find(tx, id)?.ok_or_else(|| StoreError::Internal("the new entry vanished".into()))
        })
        .await
    }

    /// Removes an entry its owner looks after and returns what it was.
    pub async fn remove_sender_list_entry(&self, owner: ListOwner, id: i64) -> Result<SenderListEntry> {
        self.write(move |tx| {
            let entry = find(tx, id)?.filter(|entry| owner.looks_after(entry.scope));
            let entry = entry.ok_or_else(|| StoreError::NotFound(format!("list entry {id}")))?;
            tx.execute("DELETE FROM sender_lists WHERE id = ?1", [id])?;
            Ok(entry)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    #[test]
    fn values_are_guessed_and_normalized() {
        let normalized = |value: &str| {
            let kind = guess_sender_kind(value);
            (kind, normalize_sender(kind, value).map_err(|err| err.to_string()))
        };
        assert_eq!(normalized("192.0.2.77"), (SenderKind::Ip, Ok("192.0.2.77".into())));
        assert_eq!(normalized(" 192.0.2.77/24 "), (SenderKind::Ip, Ok("192.0.2.0/24".into())));
        assert_eq!(normalized("::ffff:192.0.2.1"), (SenderKind::Ip, Ok("192.0.2.1".into())));
        assert_eq!(normalized("2001:DB8::1/48"), (SenderKind::Ip, Ok("2001:db8::/48".into())));
        assert!(normalized("10.0.0.0/4").1.is_err(), "too wide");
        assert_eq!(normalized("Leni@Example.DE"), (SenderKind::Address, Ok("leni@example.de".into())));
        assert_eq!(normalized("*.Mail.Example.com."), (SenderKind::Host, Ok("*.mail.example.com".into())));
        assert_eq!(normalized("@bücher.de"), (SenderKind::Domain, Ok("xn--bcher-kva.de".into())));
        assert!(normalized("com").1.is_err(), "a bare top-level domain matches too much");
        assert_eq!(normalized("*.RU"), (SenderKind::Pattern, Ok("*.ru".into())));
        assert_eq!(normalized("**Newsletter**"), (SenderKind::Pattern, Ok("*newsletter*".into())));
        assert_eq!(normalized("*@Example.com"), (SenderKind::Pattern, Ok("*@example.com".into())));
        assert!(normalized("*.c*").1.is_err(), "matches nearly everyone");
        assert!(normalized("*spam mail*").1.is_err());
        assert!(normalize_sender(SenderKind::Pattern, "spam@example.com").is_err(), "no '*'");
        assert!(normalized("not an address@").1.is_err());
        assert_eq!(normalize_sender(SenderKind::Host, "mx1.example.org").unwrap(), "mx1.example.org");
        assert_eq!(normalize_sender(SenderKind::Domain, "*.example.org").unwrap(), "example.org");
    }

    #[test]
    fn patterns_match_the_whole_text() {
        assert!(pattern_matches("*.ru", "anna@shop.ru"));
        assert!(!pattern_matches("*.ru", "anna@shop.ru.example.com"));
        assert!(pattern_matches("*newsletter*", "newsletter@example.com"));
        assert!(pattern_matches("*newsletter*", "news@newsletter.example.com"));
        assert!(pattern_matches("*@example.com", "a@example.com"));
        assert!(!pattern_matches("*@example.com", "a@mail.example.com"));
        assert!(pattern_matches("info@*.example.com", "info@mail.example.com"));
        assert!(!pattern_matches("info@*.example.com", "sales@mail.example.com"));
        assert!(pattern_matches("a*a", "aa"), "the ends may not overlap in the middle");
        assert!(!pattern_matches("ab*ba", "aba"), "but they may not share letters either");
        assert!(pattern_matches("*spam*spam*", "spamxspam"));
        assert!(!pattern_matches("*spam*spam*", "spam"));
    }

    #[tokio::test]
    async fn entries_belong_to_their_scope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let domain = store.create_domain("example.de").await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let account = NewAccount {
            address: "leni@example.de".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let leni = store.create_account(account).await.unwrap().id;
        let add = |scope, list, value: &str| NewSenderListEntry {
            scope,
            list,
            kind: None,
            value: value.into(),
            note: String::new(),
            created_by: String::new(),
            expires_at: None,
        };

        store.add_sender_list_entry(add(ListScope::Server, SenderList::Block, "198.51.100.0/24")).await.unwrap();
        let org = store.domain("example.org").await.unwrap().unwrap().id;
        store.add_sender_list_entry(add(ListScope::Domain(org), SenderList::Allow, "news.example")).await.unwrap();
        let own = store
            .add_sender_list_entry(add(ListScope::Account(leni), SenderList::Allow, "Oma@Example.net"))
            .await
            .unwrap();
        assert_eq!((own.kind, own.value.as_str()), (SenderKind::Address, "oma@example.net"));

        let again =
            store.add_sender_list_entry(add(ListScope::Account(leni), SenderList::Block, "oma@example.net")).await;
        assert!(matches!(again, Err(StoreError::Rule { code: "senderListed", .. })));
        // Someone else's scope may list the same value.
        store
            .add_sender_list_entry(add(ListScope::Domain(domain.id), SenderList::Block, "oma@example.net"))
            .await
            .unwrap();

        let for_leni = store.sender_lists_for(vec!["example.de".into()], vec![leni]).await.unwrap();
        assert_eq!(for_leni.len(), 3, "the server's, example.de's and Leni's own, not example.org's");
        assert!(store.sender_lists_for(vec![], vec![]).await.unwrap().len() == 1);
        assert_eq!(store.admin_sender_lists().await.unwrap().len(), 3);
        assert_eq!(store.sender_list(ListScope::Account(leni)).await.unwrap().len(), 1);

        let stranger = store.remove_sender_list_entry(ListOwner::Account(leni + 1), own.id).await;
        assert!(matches!(stranger, Err(StoreError::NotFound(_))), "nobody removes someone else's entry");
        store.remove_sender_list_entry(ListOwner::Account(leni), own.id).await.unwrap();

        store.delete_domain("example.org").await.unwrap();
        assert_eq!(store.admin_sender_lists().await.unwrap().len(), 2, "a removed domain takes its entries along");
    }
}
