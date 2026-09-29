//! Profile pictures (docs/profile-pictures.md): one per account and group, one logo per domain, who
//! may see them, the Face pictures that came with mail, and what Libravatar answers for.
//!
//! The pictures arrive here already decoded and written anew by the caller; this only keeps them.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::address::{base_local_part, normalize_address};
use crate::db::{get_setting, next_modseq, record_change};
use crate::{Result, Store, StoreError, now};

/// The server-wide switch: `false` forbids public pictures everywhere. Allowed when unset.
pub const PUBLIC_PICTURES_SETTING: &str = "pictures.public";
/// Face pictures kept from incoming mail; the oldest go first.
pub const MAX_RECEIVED_FACES: i64 = 20_000;
/// Of those, the most one sending domain keeps, so one domain cannot push out everyone else's.
pub const MAX_RECEIVED_FACES_PER_DOMAIN: i64 = 200;

/// Who sees a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PictureVisibility {
    /// Nobody but its owner.
    Off,
    /// People of this server.
    #[default]
    Server,
    /// Everyone: also over Libravatar and the `Face:` header.
    Public,
}

impl PictureVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            PictureVisibility::Off => "off",
            PictureVisibility::Server => "server",
            PictureVisibility::Public => "public",
        }
    }

    pub fn parse(value: &str) -> Option<PictureVisibility> {
        match value {
            "off" => Some(PictureVisibility::Off),
            "server" => Some(PictureVisibility::Server),
            "public" => Some(PictureVisibility::Public),
            _ => None,
        }
    }
}

/// Whose picture it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureOwner {
    /// A person, a service or a shared mailbox.
    Account(i64),
    Group(i64),
    /// A domain's logo.
    Domain(i64),
}

impl PictureOwner {
    fn column(self) -> (&'static str, i64) {
        match self {
            PictureOwner::Account(id) => ("account_id", id),
            PictureOwner::Group(id) => ("group_id", id),
            PictureOwner::Domain(id) => ("domain_id", id),
        }
    }
}

/// A picture to keep, already decoded and written anew.
#[derive(Debug, Clone)]
pub struct NewPicture {
    pub bytes: Vec<u8>,
    pub media_type: String,
    /// The Face PNG of the same picture; accounts only.
    pub face: Option<Vec<u8>>,
}

/// What is known about a stored picture without its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PictureMeta {
    /// SHA-256 of the picture, hex.
    pub hash: String,
    pub media_type: String,
    pub size: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPicture {
    pub hash: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub updated_at: i64,
}

/// An account's picture and what it lets others see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSettings {
    pub picture: Option<PictureMeta>,
    /// As the account chose it; see [`ProfileSettings::effective_visibility`].
    pub visibility: PictureVisibility,
    pub send_face: bool,
    /// Whether the server and the account's domain allow public pictures.
    pub may_be_public: bool,
    /// The state of JMAP's ProfilePicture.
    pub state: i64,
}

impl ProfileSettings {
    /// The visibility that holds: public counts as server-only while an admin forbids it.
    pub fn effective_visibility(&self) -> PictureVisibility {
        match self.visibility {
            PictureVisibility::Public if !self.may_be_public => PictureVisibility::Server,
            other => other,
        }
    }
}

/// A change of an account's picture settings; `None` leaves a part as it is.
#[derive(Debug, Clone, Default)]
pub struct ProfileUpdate {
    /// `Some(None)` removes the picture.
    pub picture: Option<Option<NewPicture>>,
    pub visibility: Option<PictureVisibility>,
    pub send_face: Option<bool>,
}

/// A group's picture and who sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPicture {
    pub picture: Option<PictureMeta>,
    pub visibility: PictureVisibility,
    pub may_be_public: bool,
}

/// What this server has for an address of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressPicture {
    /// Not an address of one of our domains.
    NotLocal,
    /// A masked address: it never gets a picture nor gives one away.
    Masked,
    /// The picture of the account or group behind the address, visible to people here.
    Person(StoredPicture),
    /// Nobody's picture, but the domain's logo.
    Logo(StoredPicture),
    Nothing,
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

fn server_allows_public(conn: &Connection) -> Result<bool> {
    Ok(get_setting(conn, PUBLIC_PICTURES_SETTING)?.is_none_or(|value| value != "false"))
}

fn domain_allows_public(conn: &Connection, domain_id: i64) -> Result<bool> {
    Ok(conn
        .query_row("SELECT public_pictures FROM domains WHERE id = ?1", [domain_id], |row| row.get::<_, bool>(0))
        .optional()?
        .unwrap_or(false))
}

/// Whether the server and the domain of the account's own address allow its picture to be public.
fn account_may_be_public(conn: &Connection, account_id: i64) -> Result<bool> {
    if !server_allows_public(conn)? {
        return Ok(false);
    }
    let domain: Option<bool> = conn
        .query_row(
            "SELECT d.public_pictures FROM addresses a JOIN domains d ON d.id = a.domain_id
             WHERE a.account_id = ?1 AND a.kind = 'primary'",
            [account_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(domain.unwrap_or(true))
}

fn group_domain(conn: &Connection, group_id: i64) -> Result<i64> {
    conn.query_row("SELECT domain_id FROM groups WHERE id = ?1", [group_id], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("group {group_id}")))
}

fn meta(conn: &Connection, owner: PictureOwner) -> Result<Option<PictureMeta>> {
    let (column, id) = owner.column();
    Ok(conn
        .query_row(
            &format!("SELECT hash, media_type, length(data), updated_at FROM profile_pictures WHERE {column} = ?1"),
            [id],
            |row| {
                Ok(PictureMeta {
                    hash: row.get(0)?,
                    media_type: row.get(1)?,
                    size: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn load(conn: &Connection, owner: PictureOwner) -> Result<Option<StoredPicture>> {
    let (column, id) = owner.column();
    Ok(conn
        .query_row(
            &format!("SELECT hash, media_type, data, updated_at FROM profile_pictures WHERE {column} = ?1"),
            [id],
            |row| {
                Ok(StoredPicture {
                    hash: row.get(0)?,
                    media_type: row.get(1)?,
                    bytes: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Puts a picture in place or removes it. Returns whether anything changed.
fn put(tx: &Transaction<'_>, owner: PictureOwner, picture: Option<&NewPicture>) -> Result<bool> {
    let (column, id) = owner.column();
    let Some(picture) = picture else {
        return Ok(tx.execute(&format!("DELETE FROM profile_pictures WHERE {column} = ?1"), [id])? > 0);
    };
    let hash = hex::encode(Sha256::digest(&picture.bytes));
    let face = match owner {
        PictureOwner::Account(_) => picture.face.as_deref(),
        _ => None,
    };
    tx.execute(
        &format!(
            "INSERT INTO profile_pictures ({column}, media_type, data, face, hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT ({column}) DO UPDATE SET media_type = excluded.media_type, data = excluded.data,
                 face = excluded.face, hash = excluded.hash, updated_at = excluded.updated_at"
        ),
        params![id, picture.media_type, picture.bytes, face, hash, now()],
    )?;
    Ok(true)
}

/// A new state for the account's ProfilePicture, and a change push listeners see.
fn bump_state(tx: &Transaction<'_>, account_id: i64) -> Result<i64> {
    let modseq = next_modseq(tx, account_id)?;
    tx.execute("UPDATE accounts SET picture_modseq = ?1 WHERE id = ?2", params![modseq, account_id])?;
    record_change(tx, account_id, modseq, "ProfilePicture", 0, "updated")?;
    Ok(modseq)
}

fn settings(conn: &Connection, account_id: i64) -> Result<ProfileSettings> {
    let (visibility, send_face, state): (String, bool, i64) = conn
        .query_row(
            "SELECT picture_visibility, send_face, picture_modseq FROM accounts WHERE id = ?1",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
    Ok(ProfileSettings {
        picture: meta(conn, PictureOwner::Account(account_id))?,
        visibility: PictureVisibility::parse(&visibility).unwrap_or_default(),
        send_face,
        may_be_public: account_may_be_public(conn, account_id)?,
        state,
    })
}

/// A new state for every account whose chosen public visibility the change of a switch turns on or
/// off, so their apps learn that `mayBePublic` changed.
fn bump_public_accounts(tx: &Transaction<'_>, domain_id: Option<i64>) -> Result<Vec<(i64, i64)>> {
    let accounts: Vec<i64> = tx
        .prepare(
            "SELECT acc.id FROM accounts acc
             WHERE acc.picture_visibility = 'public' AND acc.deleted_at IS NULL
               AND (?1 IS NULL OR acc.id IN (SELECT account_id FROM addresses
                                             WHERE kind = 'primary' AND domain_id = ?1))",
        )?
        .query_map([domain_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    accounts.into_iter().map(|id| Ok((id, bump_state(tx, id)?))).collect()
}

impl Store {
    /// An account's picture settings.
    pub async fn profile_settings(&self, account_id: i64) -> Result<ProfileSettings> {
        self.read(move |conn| settings(conn, account_id)).await
    }

    /// Changes an account's picture and who sees it, all of it or nothing. Public visibility while
    /// the server or the account's domain forbids it is the rule `publicNotAllowed`.
    pub async fn update_profile(&self, account_id: i64, update: ProfileUpdate) -> Result<ProfileSettings> {
        let (settings, modseq) = self
            .write(move |tx| {
                let before = settings(tx, account_id)?;
                if update.visibility == Some(PictureVisibility::Public) && !before.may_be_public {
                    return Err(rule("publicNotAllowed", "public pictures are not allowed here"));
                }
                if let Some(visibility) = update.visibility {
                    tx.execute(
                        "UPDATE accounts SET picture_visibility = ?1 WHERE id = ?2",
                        params![visibility.as_str(), account_id],
                    )?;
                }
                if let Some(send_face) = update.send_face {
                    tx.execute("UPDATE accounts SET send_face = ?1 WHERE id = ?2", params![send_face, account_id])?;
                }
                if let Some(picture) = &update.picture {
                    put(tx, PictureOwner::Account(account_id), picture.as_ref())?;
                }
                let modseq = bump_state(tx, account_id)?;
                Ok((settings(tx, account_id)?, modseq))
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(settings)
    }

    /// The picture itself, whoever may see it; for its owner and for admins.
    pub async fn picture(&self, owner: PictureOwner) -> Result<Option<StoredPicture>> {
        self.read(move |conn| load(conn, owner)).await
    }

    /// Puts a group's or a domain's picture in place, or an account's for an admin. `None` removes it.
    pub async fn set_picture(&self, owner: PictureOwner, picture: Option<NewPicture>) -> Result<bool> {
        let (changed, bumped) = self
            .write(move |tx| {
                match owner {
                    PictureOwner::Group(id) => {
                        group_domain(tx, id)?;
                    }
                    PictureOwner::Domain(id) => {
                        tx.query_row("SELECT id FROM domains WHERE id = ?1", [id], |row| row.get::<_, i64>(0))
                            .optional()?
                            .ok_or_else(|| StoreError::NotFound(format!("domain {id}")))?;
                    }
                    PictureOwner::Account(id) => {
                        settings(tx, id)?;
                    }
                }
                let changed = put(tx, owner, picture.as_ref())?;
                let bumped = match owner {
                    PictureOwner::Account(id) if changed => Some((id, bump_state(tx, id)?)),
                    _ => None,
                };
                Ok((changed, bumped))
            })
            .await?;
        if let Some((account_id, modseq)) = bumped {
            self.notify_change(account_id, modseq);
        }
        Ok(changed)
    }

    /// A group's picture and who sees it.
    pub async fn group_picture(&self, group_id: i64) -> Result<GroupPicture> {
        self.read(move |conn| {
            let domain_id = group_domain(conn, group_id)?;
            let visibility: String =
                conn.query_row("SELECT picture_visibility FROM groups WHERE id = ?1", [group_id], |row| row.get(0))?;
            Ok(GroupPicture {
                picture: meta(conn, PictureOwner::Group(group_id))?,
                visibility: PictureVisibility::parse(&visibility).unwrap_or_default(),
                may_be_public: server_allows_public(conn)? && domain_allows_public(conn, domain_id)?,
            })
        })
        .await
    }

    /// Who sees a group's picture. Public while forbidden is the rule `publicNotAllowed`.
    pub async fn set_group_picture_visibility(&self, group_id: i64, visibility: PictureVisibility) -> Result<()> {
        self.write(move |tx| {
            let domain_id = group_domain(tx, group_id)?;
            if visibility == PictureVisibility::Public
                && !(server_allows_public(tx)? && domain_allows_public(tx, domain_id)?)
            {
                return Err(rule("publicNotAllowed", "public pictures are not allowed here"));
            }
            tx.execute(
                "UPDATE groups SET picture_visibility = ?1 WHERE id = ?2",
                params![visibility.as_str(), group_id],
            )?;
            Ok(())
        })
        .await
    }

    /// The logo of a domain, without its bytes.
    pub async fn domain_logo(&self, domain_id: i64) -> Result<Option<PictureMeta>> {
        self.read(move |conn| meta(conn, PictureOwner::Domain(domain_id))).await
    }

    /// Whether the server allows public pictures at all.
    pub async fn public_pictures_allowed(&self) -> Result<bool> {
        self.read(server_allows_public).await
    }

    /// Allows or forbids public pictures for the whole server.
    pub async fn set_public_pictures_allowed(&self, allowed: bool) -> Result<()> {
        let bumped = self
            .write(move |tx| {
                if server_allows_public(tx)? == allowed {
                    return Ok(Vec::new());
                }
                crate::db::set_setting(tx, PUBLIC_PICTURES_SETTING, if allowed { "true" } else { "false" })?;
                bump_public_accounts(tx, None)
            })
            .await?;
        for (account_id, modseq) in bumped {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }

    /// Whether a domain allows public pictures of its addresses (the server may still forbid them).
    pub async fn domain_public_pictures(&self, domain_id: i64) -> Result<bool> {
        self.read(move |conn| domain_allows_public(conn, domain_id)).await
    }

    /// Allows or forbids public pictures for a domain's addresses.
    pub async fn set_domain_public_pictures(&self, domain_id: i64, allowed: bool) -> Result<()> {
        let bumped = self
            .write(move |tx| {
                let changed = tx.execute(
                    "UPDATE domains SET public_pictures = ?1 WHERE id = ?2 AND public_pictures <> ?1",
                    params![allowed, domain_id],
                )?;
                if changed == 0 {
                    tx.query_row("SELECT id FROM domains WHERE id = ?1", [domain_id], |row| row.get::<_, i64>(0))
                        .optional()?
                        .ok_or_else(|| StoreError::NotFound(format!("domain {domain_id}")))?;
                    return Ok(Vec::new());
                }
                bump_public_accounts(tx, Some(domain_id))
            })
            .await?;
        for (account_id, modseq) in bumped {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }

    /// What this server shows for one of its own addresses: the picture of whoever it belongs to,
    /// when they let people here see it, or else the domain's logo. With `logo_only`, just the logo.
    /// Masked addresses get nothing, not even the logo.
    pub async fn address_picture(&self, email: &str, logo_only: bool) -> Result<AddressPicture> {
        let Ok((local, domain)) = normalize_address(email) else {
            return Ok(AddressPicture::NotLocal);
        };
        self.read(move |conn| {
            let Some(domain_id): Option<i64> =
                conn.query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?
            else {
                return Ok(AddressPicture::NotLocal);
            };
            let base = base_local_part(&local).to_owned();
            let masked: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM masked_addresses WHERE domain_id = ?1 AND local_part IN (?2, ?3))",
                params![domain_id, local, base],
                |row| row.get(0),
            )?;
            if masked {
                return Ok(AddressPicture::Masked);
            }
            if !logo_only {
                let account: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT acc.id, acc.picture_visibility FROM addresses a JOIN accounts acc ON acc.id = a.account_id
                         WHERE a.domain_id = ?1 AND a.local_part IN (?2, ?3) AND acc.deleted_at IS NULL
                         ORDER BY a.local_part = ?2 DESC LIMIT 1",
                        params![domain_id, local, base],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let group: Option<(i64, String)> = if account.is_none() {
                    conn.query_row(
                        "SELECT id, picture_visibility FROM groups WHERE domain_id = ?1 AND local_part = ?2",
                        params![domain_id, local],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                } else {
                    None
                };
                let owner = match (account, group) {
                    (Some((id, visibility)), _) => Some((PictureOwner::Account(id), visibility)),
                    (None, Some((id, visibility))) => Some((PictureOwner::Group(id), visibility)),
                    (None, None) => None,
                };
                if let Some((owner, visibility)) = owner
                    && visibility != "off"
                    && let Some(picture) = load(conn, owner)?
                {
                    return Ok(AddressPicture::Person(picture));
                }
            }
            Ok(match load(conn, PictureOwner::Domain(domain_id))? {
                Some(logo) => AddressPicture::Logo(logo),
                None => AddressPicture::Nothing,
            })
        })
        .await
    }

    /// The Face PNG for mail an account sends from `from`: only for a person who switched it on,
    /// whose picture is public and allowed to be, sending from their own address or an alias of it —
    /// never from a masked address, a shared mailbox, a group or anyone else's address.
    pub async fn sender_face(&self, account_id: i64, from: &str) -> Result<Option<Vec<u8>>> {
        let Ok((local, domain)) = normalize_address(from) else {
            return Ok(None);
        };
        self.read(move |conn| {
            let face: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT p.face FROM accounts acc JOIN profile_pictures p ON p.account_id = acc.id
                     WHERE acc.id = ?1 AND acc.kind = 'person' AND acc.shared_mailbox = 0 AND acc.send_face = 1
                       AND acc.picture_visibility = 'public' AND acc.deleted_at IS NULL AND p.face IS NOT NULL",
                    [account_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(face) = face else { return Ok(None) };
            let Some((domain_id, domain_allows)): Option<(i64, bool)> = conn
                .query_row("SELECT id, public_pictures FROM domains WHERE name = ?1", [&domain], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?
            else {
                return Ok(None);
            };
            let base = base_local_part(&local).to_owned();
            let own: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE account_id = ?1 AND domain_id = ?2
                                AND local_part IN (?3, ?4))",
                params![account_id, domain_id, local, base],
                |row| row.get(0),
            )?;
            let allowed = domain_allows && account_may_be_public(conn, account_id)?;
            Ok((own && allowed).then_some(face))
        })
        .await
    }

    /// Keeps the Face picture of mail from `email`, which the caller checked; replaces an older one.
    pub async fn store_received_face(&self, email: &str, png: Vec<u8>) -> Result<()> {
        let email = email.trim().to_lowercase();
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO received_faces (email, png, received_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (email) DO UPDATE SET png = excluded.png, received_at = excluded.received_at",
                params![email, png, now()],
            )?;
            if let Some((_, domain)) = email.rsplit_once('@') {
                let suffix = format!("@{domain}");
                tx.execute(
                    "DELETE FROM received_faces WHERE email IN (
                         SELECT email FROM received_faces WHERE substr(email, -length(?1)) = ?1
                         ORDER BY received_at DESC, email DESC LIMIT -1 OFFSET ?2)",
                    params![suffix, MAX_RECEIVED_FACES_PER_DOMAIN],
                )?;
            }
            tx.execute(
                "DELETE FROM received_faces WHERE email IN (
                     SELECT email FROM received_faces ORDER BY received_at, email
                     LIMIT max(0, (SELECT count(*) FROM received_faces) - ?1))",
                [MAX_RECEIVED_FACES],
            )?;
            Ok(())
        })
        .await
    }

    /// The newest Face picture that came with mail from `email`, and when.
    pub async fn received_face(&self, email: &str) -> Result<Option<(Vec<u8>, i64)>> {
        let email = email.trim().to_lowercase();
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT png, received_at FROM received_faces WHERE email = ?1", [email], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?)
        })
        .await
    }

    /// Counts every change of what Libravatar may answer for; see [`Store::public_avatars`].
    pub async fn avatar_version(&self) -> Result<i64> {
        self.read(|conn| Ok(conn.query_row("SELECT value FROM avatar_version", [], |row| row.get(0))?)).await
    }

    /// Every address whose picture is public, with the picture that stands for it: the account's or
    /// group's own, or for a person without one the domain's logo. Logins and aliases only — masked
    /// and forwarding addresses are not in the address table and so never here.
    pub async fn public_avatars(&self) -> Result<Vec<(String, PictureOwner)>> {
        self.read(|conn| {
            if !server_allows_public(conn)? {
                return Ok(Vec::new());
            }
            let mut found = Vec::new();
            let mut accounts = conn.prepare(
                "SELECT a.local_part || '@' || d.name, acc.id, acc.kind, d.id, p.id IS NOT NULL, l.id IS NOT NULL
                 FROM addresses a
                 JOIN domains d ON d.id = a.domain_id AND d.public_pictures = 1
                 JOIN accounts acc ON acc.id = a.account_id
                 JOIN addresses own ON own.account_id = acc.id AND own.kind = 'primary'
                 JOIN domains od ON od.id = own.domain_id AND od.public_pictures = 1
                 LEFT JOIN profile_pictures p ON p.account_id = acc.id
                 LEFT JOIN profile_pictures l ON l.domain_id = d.id
                 WHERE acc.picture_visibility = 'public' AND acc.disabled = 0 AND acc.deleted_at IS NULL",
            )?;
            let rows = accounts.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, bool>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?;
            for row in rows {
                let (address, account, kind, domain, own, logo) = row?;
                if own {
                    found.push((address, PictureOwner::Account(account)));
                } else if logo && kind == "person" {
                    found.push((address, PictureOwner::Domain(domain)));
                }
            }
            let mut groups = conn.prepare(
                "SELECT g.local_part || '@' || d.name, g.id FROM groups g
                 JOIN domains d ON d.id = g.domain_id AND d.public_pictures = 1
                 JOIN profile_pictures p ON p.group_id = g.id
                 WHERE g.picture_visibility = 'public'",
            )?;
            let rows = groups.query_map([], |row| Ok((row.get::<_, String>(0)?, PictureOwner::Group(row.get(1)?))))?;
            for row in rows {
                found.push(row?);
            }
            Ok(found)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    fn picture(tag: &[u8]) -> NewPicture {
        NewPicture { bytes: tag.to_vec(), media_type: "image/jpeg".into(), face: Some(b"face".to_vec()) }
    }

    async fn account(store: &Store, address: &str) -> i64 {
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

    #[tokio::test]
    async fn visibility_and_the_switches_that_limit_it() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = account(&store, "mini@example.org").await;
        let fresh = store.profile_settings(mini).await.unwrap();
        assert_eq!((fresh.visibility, fresh.send_face, fresh.may_be_public), (PictureVisibility::Server, false, true));

        let update = ProfileUpdate {
            picture: Some(Some(picture(b"one"))),
            visibility: Some(PictureVisibility::Public),
            send_face: Some(true),
        };
        let set = store.update_profile(mini, update).await.unwrap();
        assert!(set.state > fresh.state);
        assert_eq!(set.picture.as_ref().unwrap().hash, hex::encode(Sha256::digest(b"one")));
        assert_eq!(store.sender_face(mini, "Mini@example.org").await.unwrap().as_deref(), Some(&b"face"[..]));
        assert_eq!(
            store.public_avatars().await.unwrap(),
            vec![("mini@example.org".into(), PictureOwner::Account(mini))]
        );
        // A disabled account is not answered for until it is enabled again.
        store.set_account_disabled("mini@example.org", true).await.unwrap();
        assert!(store.public_avatars().await.unwrap().is_empty());
        store.set_account_disabled("mini@example.org", false).await.unwrap();
        assert_eq!(store.public_avatars().await.unwrap().len(), 1);

        // Forbidding public pictures for the domain keeps the choice but not its effect.
        let domain = store.domain("example.org").await.unwrap().unwrap().id;
        store.set_domain_public_pictures(domain, false).await.unwrap();
        let limited = store.profile_settings(mini).await.unwrap();
        assert_eq!(
            (limited.visibility, limited.effective_visibility()),
            (PictureVisibility::Public, PictureVisibility::Server)
        );
        assert!(limited.state > set.state, "apps learn that mayBePublic changed");
        assert!(store.sender_face(mini, "mini@example.org").await.unwrap().is_none());
        assert!(store.public_avatars().await.unwrap().is_empty());
        let refused = store
            .update_profile(mini, ProfileUpdate { visibility: Some(PictureVisibility::Public), ..Default::default() });
        assert!(matches!(refused.await, Err(StoreError::Rule { code: "publicNotAllowed", .. })));

        store.set_domain_public_pictures(domain, true).await.unwrap();
        store.set_public_pictures_allowed(false).await.unwrap();
        assert!(!store.profile_settings(mini).await.unwrap().may_be_public);
        assert!(store.public_avatars().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_own_address_gets_a_face() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = account(&store, "mini@example.org").await;
        store.add_alias("hello@example.org", "mini@example.org").await.unwrap();
        let update = ProfileUpdate {
            picture: Some(Some(picture(b"one"))),
            visibility: Some(PictureVisibility::Public),
            send_face: Some(true),
        };
        store.update_profile(mini, update).await.unwrap();
        assert!(store.sender_face(mini, "hello@example.org").await.unwrap().is_some());
        assert!(store.sender_face(mini, "mini+news@example.org").await.unwrap().is_some());
        assert!(store.sender_face(mini, "someone@example.org").await.unwrap().is_none());
        assert!(store.sender_face(mini, "mini@elsewhere.example").await.unwrap().is_none());
        store.update_profile(mini, ProfileUpdate { send_face: Some(false), ..Default::default() }).await.unwrap();
        assert!(store.sender_face(mini, "mini@example.org").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn addresses_find_their_picture_or_the_logo() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let domain = store.domain("example.org").await.unwrap().unwrap().id;
        let mini = account(&store, "mini@example.org").await;
        let ami = account(&store, "ami@example.org").await;
        store
            .update_profile(mini, ProfileUpdate { picture: Some(Some(picture(b"mini"))), ..Default::default() })
            .await
            .unwrap();
        store
            .update_profile(
                ami,
                ProfileUpdate {
                    picture: Some(Some(picture(b"ami"))),
                    visibility: Some(PictureVisibility::Off),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store.set_picture(PictureOwner::Domain(domain), Some(picture(b"logo"))).await.unwrap();

        assert!(
            matches!(store.address_picture("Mini@Example.org", false).await.unwrap(), AddressPicture::Person(p) if p.bytes == b"mini")
        );
        assert!(
            matches!(store.address_picture("ami@example.org", false).await.unwrap(), AddressPicture::Logo(p) if p.bytes == b"logo")
        );
        assert!(matches!(store.address_picture("mini@example.org", true).await.unwrap(), AddressPicture::Logo(_)));
        assert!(matches!(store.address_picture("nobody@example.org", false).await.unwrap(), AddressPicture::Logo(_)));
        assert_eq!(store.address_picture("x@elsewhere.example", false).await.unwrap(), AddressPicture::NotLocal);

        // A public person without a picture of their own is found by Libravatar with the logo.
        store
            .update_profile(
                ami,
                ProfileUpdate {
                    picture: Some(None),
                    visibility: Some(PictureVisibility::Public),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store.public_avatars().await.unwrap(),
            vec![("ami@example.org".into(), PictureOwner::Domain(domain))]
        );
    }

    #[tokio::test]
    async fn received_faces_are_bounded() {
        let (store, _dir) = store().await;
        store.store_received_face("Friend@Example.net", b"one".to_vec()).await.unwrap();
        store.store_received_face("friend@example.net", b"two".to_vec()).await.unwrap();
        assert_eq!(store.received_face("friend@example.net").await.unwrap().unwrap().0, b"two");
        store
            .write(|tx| {
                tx.execute(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
                     INSERT INTO received_faces (email, png, received_at) SELECT 'f' || i || '@d' || (i % 1000) || '.example', x'00', 0 FROM n",
                    [MAX_RECEIVED_FACES],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store.store_received_face("new@example.net", b"new".to_vec()).await.unwrap();
        let count: i64 = store
            .read(|conn| Ok(conn.query_row("SELECT count(*) FROM received_faces", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert_eq!(count, MAX_RECEIVED_FACES);
        assert!(store.received_face("new@example.net").await.unwrap().is_some());
        assert!(store.received_face("friend@example.net").await.unwrap().is_some(), "the newest stay");
    }

    #[tokio::test]
    async fn one_domain_keeps_only_so_many_faces() {
        let (store, _dir) = store().await;
        store.store_received_face("friend@example.net", b"one".to_vec()).await.unwrap();
        for n in 0..MAX_RECEIVED_FACES_PER_DOMAIN + 5 {
            store.store_received_face(&format!("p{n}@flood.example"), b"f".to_vec()).await.unwrap();
        }
        let flood: i64 = store
            .read(|conn| {
                Ok(conn.query_row(
                    "SELECT count(*) FROM received_faces WHERE email LIKE '%@flood.example'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(flood, MAX_RECEIVED_FACES_PER_DOMAIN);
        assert!(store.received_face("friend@example.net").await.unwrap().is_some(), "others stay");
    }

    #[tokio::test]
    async fn the_avatar_version_moves_with_the_directory() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let before = store.avatar_version().await.unwrap();
        let mini = account(&store, "mini@example.org").await;
        let after_account = store.avatar_version().await.unwrap();
        assert!(after_account > before);
        store.update_profile(mini, ProfileUpdate { send_face: Some(true), ..Default::default() }).await.unwrap();
        assert_eq!(store.avatar_version().await.unwrap(), after_account, "send_face changes no address");
        store
            .update_profile(mini, ProfileUpdate { visibility: Some(PictureVisibility::Public), ..Default::default() })
            .await
            .unwrap();
        assert!(store.avatar_version().await.unwrap() > after_account);
    }
}
