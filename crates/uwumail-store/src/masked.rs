//! Masked addresses: random addresses a person makes for one website each, following Fastmail's
//! MaskedEmail (docs/jmap-masked-email.md). A masked address delivers to its owner like an alias
//! while it is pending or enabled, files its mail into the Trash without a word while it is
//! disabled, and refuses mail once deleted. A deleted address is never handed out again.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::address::{base_local_part, normalize_domain};
use crate::db::{next_modseq, record_change};
use crate::identity_grants::{Granted, revoke_identities};
use crate::{Result, Store, StoreError, now, random_bytes};

/// A pending address that got no mail for this long is deleted.
pub const MASKED_PENDING_SECS: i64 = 24 * 3600;
/// How many masked addresses one account may have that are not deleted.
const MASKED_MAX_PER_ACCOUNT: i64 = 5000;
const DESCRIPTION_MAX_CHARS: usize = 200;
const FOR_DOMAIN_MAX_CHARS: usize = 200;
const URL_MAX_CHARS: usize = 2000;
const PREFIX_MAX_CHARS: usize = 64;
const CREATED_BY_MAX_CHARS: usize = 100;

/// Short, harmless words for the names, like `rainy.otter123`.
const WORDS: &[&str] = &[
    "acorn", "amber", "apple", "aspen", "badge", "bagel", "basil", "beach", "berry", "birch", "bloom", "brook",
    "cabin", "candle", "cedar", "cloud", "clover", "cocoa", "comet", "coral", "cotton", "daisy", "dune", "ember",
    "fern", "field", "finch", "flint", "frost", "garden", "ginger", "glade", "harbor", "hazel", "honey", "island",
    "ivory", "jade", "kettle", "kiwi", "lagoon", "lemon", "lilac", "linen", "maple", "meadow", "mint", "misty", "moss",
    "nectar", "nutmeg", "oak", "ocean", "olive", "orbit", "otter", "panda", "pebble", "pepper", "piano", "pine",
    "plum", "pond", "poppy", "quartz", "quill", "rainy", "raven", "reed", "river", "robin", "saffron", "sage", "sand",
    "shell", "silver", "sky", "snow", "sparrow", "spruce", "stone", "sunny", "thyme", "tide", "tulip", "velvet",
    "violet", "walnut", "willow", "wind", "wren", "zephyr",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskedState {
    /// Made, but no mail yet. Deleted after a day without any.
    Pending,
    Enabled,
    /// Mail is taken and filed into the Trash.
    Disabled,
    /// Mail is refused.
    Deleted,
}

impl MaskedState {
    pub fn as_str(self) -> &'static str {
        match self {
            MaskedState::Pending => "pending",
            MaskedState::Enabled => "enabled",
            MaskedState::Disabled => "disabled",
            MaskedState::Deleted => "deleted",
        }
    }

    pub fn parse(value: &str) -> Option<MaskedState> {
        match value {
            "pending" => Some(MaskedState::Pending),
            "enabled" => Some(MaskedState::Enabled),
            "disabled" => Some(MaskedState::Disabled),
            "deleted" => Some(MaskedState::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedAddress {
    pub id: i64,
    pub email: String,
    pub state: MaskedState,
    /// The site it is for, as an origin like `https://www.example.com`, or empty.
    pub for_domain: String,
    pub description: String,
    pub url: Option<String>,
    pub email_prefix: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub last_message_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct NewMaskedAddress {
    /// One of the domains the account may use (see masked_domains.rs); left out, its default one.
    pub domain: Option<String>,
    /// `Pending` (the default) or `Enabled`.
    pub state: Option<MaskedState>,
    pub for_domain: String,
    pub description: String,
    pub url: Option<String>,
    /// Put in front of the random part: `a-z`, `0-9` and `_`, up to 64 characters.
    pub email_prefix: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Default)]
pub struct MaskedUpdate {
    pub state: Option<MaskedState>,
    pub for_domain: Option<String>,
    pub description: Option<String>,
    pub url: Option<Option<String>>,
}

/// What delivery needs to know about a masked address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskedDelivery {
    pub id: i64,
    /// The owner, when it still exists and is not in the trash.
    pub account_id: Option<i64>,
    pub state: MaskedState,
}

impl MaskedDelivery {
    /// The account mail for it goes to: none once it is deleted or its owner is gone.
    pub fn delivers_to(&self) -> Option<i64> {
        if self.state == MaskedState::Deleted { None } else { self.account_id }
    }
}

/// The masked address that takes mail for `local`@`domain`: the address itself, or its base
/// without a `+tag`.
pub(crate) fn find(conn: &Connection, local: &str, domain_id: i64) -> Result<Option<MaskedDelivery>> {
    let lookup = |local: &str| -> Result<Option<MaskedDelivery>> {
        Ok(conn
            .query_row(
                "SELECT x.id, acc.id, x.state FROM masked_addresses x
                 LEFT JOIN accounts acc ON acc.id = x.account_id AND acc.deleted_at IS NULL
                 WHERE x.local_part = ?1 AND x.domain_id = ?2",
                params![local, domain_id],
                |row| {
                    Ok(MaskedDelivery {
                        id: row.get(0)?,
                        account_id: row.get(1)?,
                        state: MaskedState::parse(&row.get::<_, String>(2)?).unwrap_or(MaskedState::Deleted),
                    })
                },
            )
            .optional()?)
    };
    if let Some(found) = lookup(local)? {
        return Ok(Some(found));
    }
    let base = base_local_part(local);
    if base != local { lookup(base) } else { Ok(None) }
}

const COLUMNS: &str = "x.id, x.local_part || '@' || d.name, x.state, x.for_domain, x.description, x.url,
     x.email_prefix, x.created_by, x.created_at, x.last_message_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<MaskedAddress> {
    Ok(MaskedAddress {
        id: row.get(0)?,
        email: row.get(1)?,
        state: MaskedState::parse(&row.get::<_, String>(2)?).unwrap_or(MaskedState::Deleted),
        for_domain: row.get(3)?,
        description: row.get(4)?,
        url: row.get(5)?,
        email_prefix: row.get(6)?,
        created_by: row.get(7)?,
        created_at: row.get(8)?,
        last_message_at: row.get(9)?,
    })
}

fn load(conn: &Connection, account_id: i64, id: i64) -> Result<MaskedAddress> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM masked_addresses x JOIN domains d ON d.id = x.domain_id
             WHERE x.id = ?1 AND x.account_id = ?2"
        ),
        params![id, account_id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("masked address {id}")))
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

fn limited(value: &str, max: usize, what: &str) -> Result<String> {
    let value = value.trim();
    if value.chars().count() > max {
        return Err(StoreError::Invalid(format!("{what} may have at most {max} characters")));
    }
    if value.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!("{what} may not contain control characters")));
    }
    Ok(value.to_owned())
}

fn check_url(url: Option<&str>) -> Result<Option<String>> {
    let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) else { return Ok(None) };
    let url = limited(url, URL_MAX_CHARS, "a URL")?;
    if url.chars().any(char::is_whitespace) {
        return Err(StoreError::Invalid("a URL may not contain spaces".into()));
    }
    Ok(Some(url))
}

/// Lowercase letters, digits and `_`, as Fastmail allows for a prefix.
fn check_prefix(prefix: Option<&str>) -> Result<Option<String>> {
    let Some(prefix) = prefix.map(str::trim).filter(|prefix| !prefix.is_empty()) else { return Ok(None) };
    let prefix = prefix.to_lowercase();
    let valid = prefix.chars().count() <= PREFIX_MAX_CHARS
        && prefix.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(rule("maskedPrefix", "a prefix may have up to 64 letters a-z, digits and _"));
    }
    Ok(Some(prefix))
}

/// A random local part: `word.word123`, behind the prefix if there is one.
fn random_local_part(prefix: Option<&str>) -> String {
    let bytes = random_bytes::<8>();
    let pick = |index: usize| WORDS[u16::from_le_bytes([bytes[index], bytes[index + 1]]) as usize % WORDS.len()];
    let number = u16::from_le_bytes([bytes[4], bytes[5]]) % 1000;
    let random = format!("{}.{}{number:03}", pick(0), pick(2));
    match prefix {
        Some(prefix) => format!("{prefix}.{random}"),
        None => random,
    }
}

/// Records a change of a masked address for the owner's JMAP state.
fn changed(conn: &Connection, account_id: i64, id: i64, change: &str) -> Result<i64> {
    let modseq = next_modseq(conn, account_id)?;
    let column = if change == "created" { "created_modseq = ?1, updated_modseq = ?1" } else { "updated_modseq = ?1" };
    conn.execute(&format!("UPDATE masked_addresses SET {column} WHERE id = ?2"), params![modseq, id])?;
    record_change(conn, account_id, modseq, "MaskedEmail", id, change)?;
    Ok(modseq)
}

impl Store {
    /// An account's masked addresses, the deleted ones too, newest first; `ids` narrows the list.
    pub async fn masked_addresses(&self, account_id: i64, ids: Option<Vec<i64>>) -> Result<Vec<MaskedAddress>> {
        let ids_json = ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM masked_addresses x JOIN domains d ON d.id = x.domain_id
                 WHERE x.account_id = ?1 AND (?2 IS NULL OR x.id IN (SELECT value FROM json_each(?2)))
                 ORDER BY x.created_at DESC, x.id DESC"
            ))?;
            let rows = stmt.query_map(params![account_id, ids_json], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// How many masked addresses of a domain still take mail (all but the deleted ones): a domain
    /// cannot go while there are any.
    pub async fn domain_masked_address_count(&self, domain: &str) -> Result<i64> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM masked_addresses x JOIN domains d ON d.id = x.domain_id
                 WHERE d.name = ?1 AND x.state <> 'deleted'",
                [domain],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// Makes a masked address with a random name nobody had before.
    pub async fn create_masked_address(&self, account_id: i64, new: NewMaskedAddress) -> Result<MaskedAddress> {
        let state = new.state.unwrap_or(MaskedState::Pending);
        if !matches!(state, MaskedState::Pending | MaskedState::Enabled) {
            return Err(rule("maskedState", "a new masked address is pending or enabled"));
        }
        let for_domain = limited(&new.for_domain, FOR_DOMAIN_MAX_CHARS, "the site")?;
        let description = limited(&new.description, DESCRIPTION_MAX_CHARS, "a description")?;
        let url = check_url(new.url.as_deref())?;
        let prefix = check_prefix(new.email_prefix.as_deref())?;
        let created_by = limited(&new.created_by, CREATED_BY_MAX_CHARS, "the creator")?;
        // A name that is no domain at all is no domain the account may use either.
        let wanted = match new.domain.as_deref().map(str::trim).filter(|domain| !domain.is_empty()) {
            Some(domain) => Some(
                normalize_domain(domain)
                    .map_err(|_| rule("maskedDomain", format!("{domain} is not a domain for masked addresses")))?,
            ),
            None => None,
        };
        let (created, modseq) = self
            .write(move |tx| {
                let live: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ?1 AND deleted_at IS NULL)",
                    [account_id],
                    |row| row.get(0),
                )?;
                if !live {
                    return Err(StoreError::NotFound(format!("account {account_id}")));
                }
                // Where the account may make one: its own domain's policy, or what an admin set for it.
                let policy = crate::masked_domains::effective(tx, account_id)?;
                let domain_name = match &wanted {
                    Some(wanted) => policy.domains.iter().find(|name| *name == wanted),
                    None => policy.default_domain.as_ref(),
                };
                let Some(domain_name) = domain_name else {
                    return Err(rule(
                        "maskedDomain",
                        match &wanted {
                            Some(wanted) => format!("masked addresses on {wanted} are not allowed for this account"),
                            None => "masked addresses are not switched on for this account".into(),
                        },
                    ));
                };
                let domain_id = crate::directory::domain_id(tx, domain_name)?;
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM masked_addresses WHERE account_id = ?1 AND state <> 'deleted'",
                    [account_id],
                    |row| row.get(0),
                )?;
                if count >= MASKED_MAX_PER_ACCOUNT {
                    return Err(rule("maskedLimit", format!("at most {MASKED_MAX_PER_ACCOUNT} masked addresses")));
                }
                let mut local = None;
                for _ in 0..20 {
                    let candidate = random_local_part(prefix.as_deref());
                    let login_taken: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM accounts WHERE login = ?1)",
                        [format!("{candidate}@{domain_name}")],
                        |row| row.get(0),
                    )?;
                    let released: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM released_addresses WHERE local_part = ?1 AND domain_id = ?2)",
                        params![candidate, domain_id],
                        |row| row.get(0),
                    )?;
                    if !login_taken
                        && !released
                        && !crate::forward_addresses::address_in_use(tx, &candidate, domain_id)?
                    {
                        local = Some(candidate);
                        break;
                    }
                }
                let local = local.ok_or_else(|| StoreError::Internal("no free masked address found".into()))?;
                tx.execute(
                    "INSERT INTO masked_addresses (account_id, local_part, domain_id, state, for_domain, description, url,
                                                   email_prefix, created_by, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        account_id,
                        local,
                        domain_id,
                        state.as_str(),
                        for_domain,
                        description,
                        url,
                        prefix,
                        created_by,
                        now()
                    ],
                )?;
                let id = tx.last_insert_rowid();
                let modseq = changed(tx, account_id, id, "created")?;
                Ok((load(tx, account_id, id)?, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(created)
    }

    /// Changes a masked address. It can go between enabled, disabled and deleted either way, a
    /// deleted one back included, but never back to pending.
    pub async fn update_masked_address(&self, account_id: i64, id: i64, update: MaskedUpdate) -> Result<MaskedAddress> {
        let for_domain =
            update.for_domain.as_deref().map(|v| limited(v, FOR_DOMAIN_MAX_CHARS, "the site")).transpose()?;
        let description =
            update.description.as_deref().map(|v| limited(v, DESCRIPTION_MAX_CHARS, "a description")).transpose()?;
        let url = update.url.as_ref().map(|url| check_url(url.as_deref())).transpose()?;
        let (updated, modseq, granted) = self
            .write(move |tx| {
                let before = load(tx, account_id, id)?;
                if let Some(state) = update.state {
                    if state == MaskedState::Pending && before.state != MaskedState::Pending {
                        return Err(rule("maskedState", "a masked address cannot become pending again"));
                    }
                    tx.execute("UPDATE masked_addresses SET state = ?1 WHERE id = ?2", params![state.as_str(), id])?;
                }
                if let Some(for_domain) = &for_domain {
                    tx.execute("UPDATE masked_addresses SET for_domain = ?1 WHERE id = ?2", params![for_domain, id])?;
                }
                if let Some(description) = &description {
                    tx.execute("UPDATE masked_addresses SET description = ?1 WHERE id = ?2", params![description, id])?;
                }
                if let Some(url) = &url {
                    tx.execute("UPDATE masked_addresses SET url = ?1 WHERE id = ?2", params![url, id])?;
                }
                let modseq = changed(tx, account_id, id, "updated")?;
                let after = load(tx, account_id, id)?;
                // An identity made for it goes once it is deleted; nobody may send as it any more.
                let mut granted = Granted::default();
                if after.state == MaskedState::Deleted {
                    revoke_identities(tx, account_id, &after.email, &mut granted)?;
                }
                Ok((after, modseq, granted))
            })
            .await?;
        self.notify_change(account_id, modseq);
        self.notify_granted(granted);
        Ok(updated)
    }

    /// The masked address that takes mail for `address`, if it is one.
    pub async fn masked_delivery(&self, address: &str) -> Result<Option<MaskedDelivery>> {
        let Ok((local, domain)) = crate::normalize_address(address) else {
            return Ok(None);
        };
        self.read(move |conn| {
            let Some(domain_id) =
                conn.query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?
            else {
                return Ok(None);
            };
            find(conn, &local, domain_id)
        })
        .await
    }

    /// Notes that mail arrived for a masked address: a pending one is enabled by it.
    pub async fn note_masked_message(&self, id: i64) -> Result<()> {
        let changed = self
            .write(move |tx| {
                let Some(account_id): Option<i64> = tx
                    .query_row("SELECT account_id FROM masked_addresses WHERE id = ?1", [id], |row| row.get(0))
                    .optional()?
                    .flatten()
                else {
                    return Ok(None);
                };
                tx.execute(
                    "UPDATE masked_addresses SET last_message_at = ?1,
                         state = CASE state WHEN 'pending' THEN 'enabled' ELSE state END
                     WHERE id = ?2",
                    params![now(), id],
                )?;
                Ok(Some((account_id, changed(tx, account_id, id, "updated")?)))
            })
            .await?;
        if let Some((account_id, modseq)) = changed {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }

    /// Deletes pending masked addresses that got no mail within a day. Their names stay taken, as
    /// every deleted one's do. Returns how many.
    pub async fn retire_pending_masked_addresses(&self) -> Result<usize> {
        let retired = self
            .write(|tx| {
                let stale: Vec<(i64, Option<i64>)> = tx
                    .prepare(
                        "SELECT id, account_id FROM masked_addresses
                         WHERE state = 'pending' AND last_message_at IS NULL AND created_at < ?1",
                    )?
                    .query_map([now() - MASKED_PENDING_SECS], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                let mut changes = Vec::new();
                for (id, account_id) in stale {
                    tx.execute("UPDATE masked_addresses SET state = 'deleted' WHERE id = ?1", [id])?;
                    if let Some(account_id) = account_id {
                        changes.push((account_id, changed(tx, account_id, id, "updated")?));
                    }
                }
                Ok(changes)
            })
            .await?;
        let count = retired.len();
        for (account_id, modseq) in retired {
            self.notify_change(account_id, modseq);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{DomainKind, DomainMaskedPolicy, MaskedMode, NewAccount, Role};

    async fn person(store: &Store, address: &str) -> i64 {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    fn code<T: std::fmt::Debug>(result: Result<T>) -> &'static str {
        match result {
            Err(StoreError::Rule { code, .. }) => code,
            other => panic!("expected a rule, got {other:?}"),
        }
    }

    #[test]
    fn random_names_look_like_words() {
        let name = random_local_part(None);
        let (first, second) = name.split_once('.').unwrap();
        assert!(WORDS.contains(&first));
        assert!(second.len() >= 5 && second[second.len() - 3..].chars().all(|c| c.is_ascii_digit()));
        assert!(random_local_part(Some("shop")).starts_with("shop."));
        assert!(check_prefix(Some("Shop_1")).unwrap().as_deref() == Some("shop_1"));
        assert!(check_prefix(Some("shop.")).is_err() && check_prefix(Some(&"a".repeat(65))).is_err());
    }

    #[tokio::test]
    async fn masked_addresses_live_and_stay_reserved() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain_with_kind("masked.example", DomainKind::Masked).await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        assert_eq!(code(store.create_masked_address(mini, NewMaskedAddress::default()).await), "maskedDomain");
        let policy = DomainMaskedPolicy {
            mode: MaskedMode::Dedicated,
            masked_domains: vec!["masked.example".into()],
            default_domain: None,
        };
        store.set_domain_masked_policy("example.org", policy).await.unwrap();
        assert_eq!(store.effective_masked_policy(mini).await.unwrap().domains, vec!["masked.example"]);
        assert_eq!(store.domain_masked_address_count("masked.example").await.unwrap(), 0);

        let before = store.account_modseq(mini).await.unwrap();
        let new = NewMaskedAddress {
            for_domain: "https://shop.example.com".into(),
            description: "Shop".into(),
            email_prefix: Some("shop".into()),
            created_by: "portal".into(),
            ..Default::default()
        };
        let masked = store.create_masked_address(mini, new).await.unwrap();
        assert!(masked.email.starts_with("shop.") && masked.email.ends_with("@masked.example"));
        assert_eq!(masked.state, MaskedState::Pending);
        assert!(store.account_modseq(mini).await.unwrap() > before);
        assert_eq!(store.domain_masked_address_count("masked.example").await.unwrap(), 1);
        assert!(store.delete_domain("masked.example").await.is_err(), "still in use");

        // It delivers to Mini, who may send as it, and nobody may take it.
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), Some(mini));
        assert!(store.account_owns_address(mini, &masked.email).await.unwrap());
        assert!(!store.account_owns_address(leni, &masked.email).await.unwrap());
        assert!(store.add_alias(&masked.email, "leni@example.org").await.is_err());

        // The first message enables it.
        let delivery = store.masked_delivery(&masked.email).await.unwrap().unwrap();
        assert_eq!((delivery.account_id, delivery.state), (Some(mini), MaskedState::Pending));
        store.note_masked_message(delivery.id).await.unwrap();
        let list = store.masked_addresses(mini, None).await.unwrap();
        assert_eq!(list[0].state, MaskedState::Enabled);
        assert!(list[0].last_message_at.is_some());

        let disabled = MaskedUpdate { state: Some(MaskedState::Disabled), ..Default::default() };
        store.update_masked_address(mini, masked.id, disabled).await.unwrap();
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), Some(mini), "still taken, into the Trash");
        let pending = MaskedUpdate { state: Some(MaskedState::Pending), ..Default::default() };
        assert_eq!(code(store.update_masked_address(mini, masked.id, pending).await), "maskedState");
        assert!(matches!(
            store.update_masked_address(leni, masked.id, MaskedUpdate::default()).await,
            Err(StoreError::NotFound(_))
        ));

        // Deleted: no mail, and never handed out again.
        store.create_identity(mini, "Shop", &masked.email).await.unwrap();
        let deleted = MaskedUpdate { state: Some(MaskedState::Deleted), ..Default::default() };
        store.update_masked_address(mini, masked.id, deleted).await.unwrap();
        assert!(!store.identities(mini).await.unwrap().iter().any(|i| i.email == masked.email), "its identity went");
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), None);
        assert!(!store.account_owns_address(mini, &masked.email).await.unwrap());
        assert!(store.add_alias(&masked.email, "leni@example.org").await.is_err());
        store.delete_account("mini@example.org").await.unwrap();
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), None, "reserved after the owner is gone");
        assert!(store.add_alias(&masked.email, "leni@example.org").await.is_err());
    }

    #[tokio::test]
    async fn no_catch_all_takes_mail_of_a_deleted_one() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let policy = DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() };
        store.set_domain_masked_policy("example.org", policy).await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        let masked = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        assert!(masked.email.ends_with("@example.org"));
        store.set_catch_all("example.org", Some("leni@example.org")).await.unwrap();
        assert_eq!(store.resolve_recipient("ghost@example.org").await.unwrap(), Some(leni));
        let deleted = MaskedUpdate { state: Some(MaskedState::Deleted), ..Default::default() };
        store.update_masked_address(mini, masked.id, deleted).await.unwrap();
        assert_eq!(store.resolve_recipient(&masked.email).await.unwrap(), None);
    }

    #[tokio::test]
    async fn pending_addresses_without_mail_go_after_a_day() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let policy = DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() };
        store.set_domain_masked_policy("example.org", policy).await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let old = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        let fresh = store.create_masked_address(mini, NewMaskedAddress::default()).await.unwrap();
        let enabled = NewMaskedAddress { state: Some(MaskedState::Enabled), ..Default::default() };
        let kept = store.create_masked_address(mini, enabled).await.unwrap();
        assert!(old.email.ends_with("@example.org"), "the account's own domain first");
        let id = old.id;
        store
            .write(move |tx| {
                tx.execute("UPDATE masked_addresses SET created_at = created_at - 90000 WHERE id = ?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(store.retire_pending_masked_addresses().await.unwrap(), 1);
        let states: Vec<(i64, MaskedState)> =
            store.masked_addresses(mini, None).await.unwrap().into_iter().map(|m| (m.id, m.state)).collect();
        assert!(states.contains(&(old.id, MaskedState::Deleted)));
        assert!(states.contains(&(fresh.id, MaskedState::Pending)));
        assert!(states.contains(&(kept.id, MaskedState::Enabled)));
    }
}
