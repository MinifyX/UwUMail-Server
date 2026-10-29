//! What an identity provider pushes over SCIM 2.0 (docs/uwuauth.md): which account belongs to which
//! SCIM id and external id, and the groups it keeps here. The accounts themselves are made and changed
//! the usual way; this only remembers the provider's names for them.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::directory::{ACCOUNT_COLUMN_COUNT, ACCOUNT_COLUMNS, account_from_row, login_key};
use crate::{Account, NewAccount, Result, Store, StoreError, now};

/// At most this many members in one group.
pub const SCIM_MAX_MEMBERS: usize = 10_000;

/// A person as SCIM sees them.
#[derive(Debug, Clone)]
pub struct ScimUser {
    pub id: String,
    pub external_id: Option<String>,
    /// The provider switched the account off (and only it switches it back on).
    pub disabled_by_scim: bool,
    pub created: i64,
    pub updated: i64,
    pub account: Account,
}

/// Which people a SCIM search is about. Only `eq` filters exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScimUserFilter {
    All,
    Id(String),
    UserName(String),
    ExternalId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimGroup {
    pub id: String,
    pub display_name: String,
    pub external_id: Option<String>,
    pub members: Vec<String>,
    pub created: i64,
    pub updated: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScimGroupFilter {
    All,
    Id(String),
    DisplayName(String),
    ExternalId(String),
}

/// What changes in a group; `None` leaves it as it is.
#[derive(Debug, Clone, Default)]
pub struct ScimGroupUpdate {
    pub display_name: Option<String>,
    pub external_id: Option<Option<String>>,
    pub members: Option<Vec<String>>,
}

/// People SCIM knows: persons, not services or shared mailboxes, not in the trash.
const VISIBLE: &str = "kind = 'person' AND shared_mailbox = 0 AND deleted_at IS NULL";

const SCIM_JOIN: &str = "accounts JOIN (SELECT id AS s_id, account_id AS s_account, external_id AS s_ext, \
     disabled_by_scim AS s_disabled, created AS s_created, updated AS s_updated FROM scim_users) ON s_account = accounts.id";

fn user_from_row(row: &Row<'_>) -> rusqlite::Result<ScimUser> {
    let account = account_from_row(row)?;
    let at = ACCOUNT_COLUMN_COUNT;
    Ok(ScimUser {
        id: row.get(at)?,
        external_id: row.get(at + 1)?,
        disabled_by_scim: row.get(at + 2)?,
        created: row.get(at + 3)?,
        updated: row.get(at + 4)?,
        account,
    })
}

fn user_columns() -> String {
    format!("{ACCOUNT_COLUMNS}, s_id, s_ext, s_disabled, s_created, s_updated")
}

/// Gives every person who has none yet a SCIM id.
/// A count or offset as SQLite takes it: anything past `i64::MAX` is `i64::MAX`.
fn sql_int(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn ensure_ids(conn: &Connection) -> Result<()> {
    let at = now();
    conn.execute(
        &format!(
            "INSERT INTO scim_users (id, account_id, created, updated)
             SELECT lower(hex(randomblob(16))), id, ?1, ?1 FROM accounts
             WHERE {VISIBLE} AND NOT EXISTS (SELECT 1 FROM scim_users s WHERE s.account_id = accounts.id)"
        ),
        [at],
    )?;
    Ok(())
}

fn find_user(conn: &Connection, id: &str, with_trash: bool) -> Result<Option<ScimUser>> {
    let visible = if with_trash { "kind = 'person' AND shared_mailbox = 0" } else { VISIBLE };
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM {SCIM_JOIN} WHERE s_id = ?1 AND {visible}", user_columns()),
            [id],
            user_from_row,
        )
        .optional()?)
}

fn group_from_row(row: &Row<'_>) -> rusqlite::Result<ScimGroup> {
    let members: String = row.get(3)?;
    Ok(ScimGroup {
        id: row.get(0)?,
        display_name: row.get(1)?,
        external_id: row.get(2)?,
        members: serde_json::from_str(&members).unwrap_or_default(),
        created: row.get(4)?,
        updated: row.get(5)?,
    })
}

const GROUP_COLUMNS: &str = "id, display_name, external_id, members, created, updated";

fn find_group(conn: &Connection, id: &str) -> Result<Option<ScimGroup>> {
    Ok(conn
        .query_row(&format!("SELECT {GROUP_COLUMNS} FROM scim_groups WHERE id = ?1"), [id], group_from_row)
        .optional()?)
}

fn clean_members(members: Vec<String>) -> Result<String> {
    let mut members: Vec<String> = members.into_iter().map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).collect();
    members.sort();
    members.dedup();
    if members.len() > SCIM_MAX_MEMBERS {
        return Err(StoreError::Invalid(format!("a group has at most {SCIM_MAX_MEMBERS} members")));
    }
    Ok(serde_json::to_string(&members).expect("strings are JSON"))
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(StoreError::Invalid("a group needs a name of up to 200 characters".into()));
    }
    Ok(name.to_owned())
}

fn conflict_on_unique(err: rusqlite::Error, what: String) -> StoreError {
    match err {
        rusqlite::Error::SqliteFailure(failure, _) if failure.code == rusqlite::ErrorCode::ConstraintViolation => {
            StoreError::Conflict(what)
        }
        err => err.into(),
    }
}

impl Store {
    /// People matching `filter`: how many in all, and `count` of them from `start` (0-based).
    pub async fn scim_users(
        &self,
        filter: ScimUserFilter,
        start: usize,
        count: usize,
    ) -> Result<(usize, Vec<ScimUser>)> {
        self.write(move |tx| {
            ensure_ids(tx)?;
            let (condition, value) = match &filter {
                ScimUserFilter::All => ("1 = 1", String::new()),
                ScimUserFilter::Id(id) => ("s_id = ?1", id.clone()),
                ScimUserFilter::UserName(name) => ("login = ?1", login_key(name).unwrap_or_else(|_| name.trim().to_lowercase())),
                ScimUserFilter::ExternalId(id) => ("s_ext = ?1", id.clone()),
            };
            let bind = |stmt: &mut rusqlite::Statement<'_>| -> rusqlite::Result<()> {
                if filter != ScimUserFilter::All {
                    stmt.raw_bind_parameter(1, &value)?;
                }
                Ok(())
            };
            let mut total = tx.prepare(&format!("SELECT count(*) FROM {SCIM_JOIN} WHERE {VISIBLE} AND {condition}"))?;
            bind(&mut total)?;
            let total: i64 = total.raw_query().next()?.map(|row| row.get(0)).transpose()?.unwrap_or(0);
            let total = usize::try_from(total).unwrap_or(0);
            let mut stmt = tx.prepare(&format!(
                "SELECT {} FROM {SCIM_JOIN} WHERE {VISIBLE} AND {condition} ORDER BY s_created, s_id LIMIT {} OFFSET {}",
                user_columns(),
                sql_int(count),
                sql_int(start)
            ))?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut users = Vec::new();
            while let Some(row) = rows.next()? {
                users.push(user_from_row(row)?);
            }
            Ok((total, users))
        })
        .await
    }

    /// One person by SCIM id; with `with_trash` also one in the trash.
    pub async fn scim_user(&self, id: &str, with_trash: bool) -> Result<Option<ScimUser>> {
        let id = id.to_owned();
        self.write(move |tx| {
            ensure_ids(tx)?;
            find_user(tx, &id, with_trash)
        })
        .await
    }

    /// A person the provider brings: an account without a password that logs in with OpenID
    /// Connect, its SCIM id and the provider's id, and locked by SCIM if `disabled`. All or nothing:
    /// an `externalId` another person has already leaves no account behind.
    pub async fn scim_create_account(
        &self,
        new: NewAccount,
        external_id: Option<String>,
        disabled: bool,
    ) -> Result<ScimUser> {
        let new = NewAccount { password: None, ..new };
        let (_, user) = self
            .create_account_then(new, move |tx, account| {
                tx.execute(
                    "UPDATE accounts SET auth_source = 'oidc', disabled = ?1 WHERE id = ?2",
                    params![disabled, account.id],
                )?;
                ensure_ids(tx)?;
                tx.execute(
                    "UPDATE scim_users SET external_id = ?1, disabled_by_scim = ?2, updated = ?3 WHERE account_id = ?4",
                    params![external_id, disabled, now(), account.id],
                )
                .map_err(|err| conflict_on_unique(err, "externalId".into()))?;
                let id: String =
                    tx.query_row("SELECT id FROM scim_users WHERE account_id = ?1", [account.id], |row| row.get(0))?;
                find_user(tx, &id, false)?.ok_or_else(|| StoreError::NotFound(format!("user {id}")))
            })
            .await?;
        Ok(user)
    }

    /// The SCIM id of an account, made now if it has none, with the provider's id for the person.
    /// Another account with the same external id is a conflict.
    pub async fn scim_link(&self, account_id: i64, external_id: Option<String>) -> Result<ScimUser> {
        self.write(move |tx| {
            ensure_ids(tx)?;
            let id: String = tx
                .query_row("SELECT id FROM scim_users WHERE account_id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
            tx.execute(
                "UPDATE scim_users SET external_id = ?1, updated = ?2 WHERE id = ?3",
                params![external_id, now(), id],
            )
            .map_err(|err| conflict_on_unique(err, "externalId".into()))?;
            find_user(tx, &id, false)?.ok_or_else(|| StoreError::NotFound(format!("user {id}")))
        })
        .await
    }

    /// Changes the provider's id of a person.
    pub async fn scim_set_external_id(&self, id: &str, external_id: Option<String>) -> Result<()> {
        let id = id.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE scim_users SET external_id = ?1, updated = ?2 WHERE id = ?3",
                params![external_id, now(), id],
            )
            .map_err(|err| conflict_on_unique(err, "externalId".into()))?;
            Ok(())
        })
        .await
    }

    /// Notes whether the provider is the one that switched the account off, and that it changed.
    pub async fn scim_touch(&self, id: &str, disabled_by_scim: Option<bool>) -> Result<()> {
        let id = id.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE scim_users SET disabled_by_scim = coalesce(?1, disabled_by_scim), updated = ?2 WHERE id = ?3",
                params![disabled_by_scim, now(), id],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn scim_groups(
        &self,
        filter: ScimGroupFilter,
        start: usize,
        count: usize,
    ) -> Result<(usize, Vec<ScimGroup>)> {
        self.read(move |conn| {
            let (condition, value) = match &filter {
                ScimGroupFilter::All => ("1 = 1", String::new()),
                ScimGroupFilter::Id(id) => ("id = ?1", id.clone()),
                ScimGroupFilter::DisplayName(name) => ("display_name = ?1", name.trim().to_owned()),
                ScimGroupFilter::ExternalId(id) => ("external_id = ?1", id.clone()),
            };
            let bind = |stmt: &mut rusqlite::Statement<'_>| -> rusqlite::Result<()> {
                if filter != ScimGroupFilter::All {
                    stmt.raw_bind_parameter(1, &value)?;
                }
                Ok(())
            };
            let mut total = conn.prepare(&format!("SELECT count(*) FROM scim_groups WHERE {condition}"))?;
            bind(&mut total)?;
            let total: i64 = total.raw_query().next()?.map(|row| row.get(0)).transpose()?.unwrap_or(0);
            let total = usize::try_from(total).unwrap_or(0);
            let mut stmt = conn.prepare(&format!(
                "SELECT {GROUP_COLUMNS} FROM scim_groups WHERE {condition} ORDER BY created, id LIMIT {} OFFSET {}",
                sql_int(count),
                sql_int(start)
            ))?;
            bind(&mut stmt)?;
            let mut rows = stmt.raw_query();
            let mut groups = Vec::new();
            while let Some(row) = rows.next()? {
                groups.push(group_from_row(row)?);
            }
            Ok((total, groups))
        })
        .await
    }

    pub async fn scim_group(&self, id: &str) -> Result<Option<ScimGroup>> {
        let id = id.to_owned();
        self.read(move |conn| find_group(conn, &id)).await
    }

    /// A new group; one with the same name is a conflict.
    pub async fn scim_create_group(
        &self,
        display_name: &str,
        external_id: Option<String>,
        members: Vec<String>,
    ) -> Result<ScimGroup> {
        let name = clean_name(display_name)?;
        let members = clean_members(members)?;
        self.write(move |tx| {
            let at = now();
            let id: String = tx.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
            tx.execute(
                "INSERT INTO scim_groups (id, display_name, external_id, members, created, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, name, external_id, members, at],
            )
            .map_err(|err| conflict_on_unique(err, format!("group {name}")))?;
            find_group(tx, &id)?.ok_or_else(|| StoreError::NotFound(format!("group {id}")))
        })
        .await
    }

    pub async fn scim_update_group(&self, id: &str, update: ScimGroupUpdate) -> Result<ScimGroup> {
        let id = id.to_owned();
        let name = update.display_name.as_deref().map(clean_name).transpose()?;
        let members = update.members.map(clean_members).transpose()?;
        self.write(move |tx| {
            let before = find_group(tx, &id)?.ok_or_else(|| StoreError::NotFound(format!("group {id}")))?;
            let name = name.unwrap_or(before.display_name);
            let external = update.external_id.unwrap_or(before.external_id);
            let members = match members {
                Some(members) => members,
                None => serde_json::to_string(&before.members).expect("strings are JSON"),
            };
            tx.execute(
                "UPDATE scim_groups SET display_name = ?1, external_id = ?2, members = ?3, updated = ?4 WHERE id = ?5",
                params![name, external, members, now(), id],
            )
            .map_err(|err| conflict_on_unique(err, format!("group {name}")))?;
            find_group(tx, &id)?.ok_or_else(|| StoreError::NotFound(format!("group {id}")))
        })
        .await
    }

    /// Returns whether the group was there.
    pub async fn scim_delete_group(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.write(move |tx| Ok(tx.execute("DELETE FROM scim_groups WHERE id = ?1", [id])? > 0)).await
    }

    /// Forgets every group and every external id, when the provider is unpaired.
    pub async fn scim_forget(&self) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM scim_groups", [])?;
            tx.execute("UPDATE scim_users SET external_id = NULL, disabled_by_scim = 0", [])?;
            Ok(())
        })
        .await
    }
}
