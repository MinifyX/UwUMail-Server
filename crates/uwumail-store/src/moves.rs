//! Moves the admin runs: a whole domain with everyone on it, or one mailbox, copied from another
//! server (docs/moving.md, "For admins").
//!
//! A move holds what everyone shares -- the old IMAP server, how contacts and calendars are found,
//! how many mailboxes are copied at once -- and one row per mailbox with the old login and its
//! password, sealed like the passwords of fetched mailboxes. The worker in the server crate takes
//! one mailbox at a time per free slot, copies for a while and hands it back: queued again when
//! its time was up, synced when everything is here (the next round comes after `sync_minutes`),
//! or paused with a reason.
//!
//! Unlike a person's own move it does not stop when everything is here: mail keeps arriving at
//! the old server until the MX records point here. The admin finishes the move then; one last
//! round runs for every mailbox, and after it the passwords are wiped.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::address::normalize_address;
use crate::fetch::{check_host, seal, unseal};
use crate::migration_jobs::MigrationProgress;
use crate::{
    DavCollection, DavImportMode, DavImportReport, DavKind, NewDavCollection, NewImportCollection, Result, Split,
    Store, StoreError, now,
};

/// Mailboxes in one move at most.
pub const MAX_MOVE_MAILBOXES: usize = 2000;
/// Moves that are not done yet, at most.
pub const MAX_OPEN_MOVES: usize = 20;
/// Mailboxes of one move copied at once, at most; the default is gentler.
pub const MAX_MOVE_PARALLEL: i64 = 8;
pub const DEFAULT_MOVE_PARALLEL: i64 = 2;
/// How long an up-to-date mailbox waits for its next round, in minutes.
pub const MIN_MOVE_SYNC_MINUTES: i64 = 5;
pub const MAX_MOVE_SYNC_MINUTES: i64 = 24 * 60;
pub const DEFAULT_MOVE_SYNC_MINUTES: i64 = 60;
/// Longest login and password taken for an old mailbox.
const MAX_LOGIN: usize = 320;
const MAX_PASSWORD: usize = 1024;
const MAX_URL: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveKind {
    Domain,
    Mailbox,
}

impl MoveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::Mailbox => "mailbox",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "domain" => Some(Self::Domain),
            "mailbox" => Some(Self::Mailbox),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveState {
    /// Copying, and keeping up with what arrives at the old server.
    Active,
    /// Stopped by the admin.
    Paused,
    /// The last round runs; afterwards the passwords go.
    Finishing,
    /// Over: every password is wiped.
    Done,
}

impl MoveState {
    fn parse(value: &str) -> Self {
        match value {
            "paused" => Self::Paused,
            "finishing" => Self::Finishing,
            "done" => Self::Done,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveMailboxState {
    Queued,
    Running,
    /// Stopped; `error` says why (`stopped` when by hand).
    Paused,
    /// Everything there was is here; the next round comes at `next_sync_at`.
    Synced,
    /// Finished, the password is wiped.
    Done,
}

impl MoveMailboxState {
    fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "synced" => Self::Synced,
            "done" => Self::Done,
            _ => Self::Queued,
        }
    }
}

/// How contacts and calendars of the old mailboxes are found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DavMode {
    /// From the old address: known providers, the domain's SRV records and `/.well-known/`, then
    /// the IMAP server's `/.well-known/`.
    Auto,
    /// `https://<host>/remote.php/dav/`.
    Nextcloud,
    /// SOGo, as mailcow has it: `https://<host>/SOGo/dav/`.
    Sogo,
    Icloud,
    Gmx,
    Webde,
    /// The address given in `dav_url`.
    Custom,
    /// Contacts and calendars do not come along (or come as files).
    None,
}

impl DavMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nextcloud => "nextcloud",
            Self::Sogo => "sogo",
            Self::Icloud => "icloud",
            Self::Gmx => "gmx",
            Self::Webde => "webde",
            Self::Custom => "custom",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "auto" | "" => Self::Auto,
            "nextcloud" => Self::Nextcloud,
            "sogo" | "mailcow" => Self::Sogo,
            "icloud" => Self::Icloud,
            "gmx" => Self::Gmx,
            "webde" => Self::Webde,
            "custom" => Self::Custom,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// A move, as the admin page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    pub id: i64,
    pub kind: MoveKind,
    pub domain: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub dav_mode: DavMode,
    pub dav_host: String,
    pub dav_url: String,
    pub contacts: bool,
    pub calendars: bool,
    pub parallel: i64,
    pub sync_minutes: i64,
    pub state: MoveState,
    pub created_at: i64,
    pub finish_requested_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// Over all its mailboxes.
    pub summary: MoveSummary,
}

/// How a move's mailboxes stand, together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveSummary {
    pub mailboxes: i64,
    pub queued: i64,
    pub running: i64,
    pub paused: i64,
    pub synced: i64,
    pub done: i64,
    pub messages_done: i64,
    pub messages_total: i64,
    pub messages_skipped: i64,
    pub bytes_done: i64,
    pub contacts_done: i64,
    pub events_done: i64,
}

/// One mailbox of a move. The password never leaves the store this way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveMailbox {
    pub id: i64,
    pub move_id: i64,
    pub account_id: i64,
    /// The mailbox here.
    pub address: String,
    pub display_name: String,
    pub quota_bytes: i64,
    pub used_bytes: i64,
    pub old_address: String,
    pub login: String,
    /// Its own old server; empty when it is the move's.
    pub imap_host: String,
    pub imap_port: u16,
    pub dav_url: String,
    /// Made by this move (and invited), rather than a mailbox that was there.
    pub created_account: bool,
    /// Whether the password for the old mailbox is still kept.
    pub has_password: bool,
    pub state: MoveMailboxState,
    pub final_round: bool,
    /// `quotaExceeded`, `loginRefused`, `unreachable`, `notPublic`, `noMailbox`, `stopped`,
    /// `failed`; empty otherwise.
    pub error: String,
    pub error_detail: String,
    #[serde(flatten)]
    pub progress: MigrationProgress,
    pub source_bytes: Option<i64>,
    pub contacts_done: i64,
    pub events_done: i64,
    pub dav_error: String,
    /// The collections found in the first round, as JSON; the worker asks them again.
    #[serde(skip)]
    pub dav_found: String,
    pub rounds: i64,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub last_synced_at: Option<i64>,
    pub next_sync_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl MoveMailbox {
    /// What `import_progress` remembers the folders under; the same as a person's own move from
    /// the same login at the same host, so the two never copy anything twice.
    pub fn source_name(&self, host: &str) -> String {
        format!("move:{}@{}", self.login, host)
    }
}

#[derive(Debug, Clone)]
pub struct NewMove {
    pub kind: MoveKind,
    pub domain: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub dav_mode: DavMode,
    pub dav_host: String,
    pub dav_url: String,
    pub contacts: bool,
    pub calendars: bool,
    pub parallel: i64,
    pub sync_minutes: i64,
    pub created_by: Option<i64>,
}

#[derive(Clone)]
pub struct NewMoveMailbox {
    pub account_id: i64,
    pub old_address: String,
    pub login: String,
    pub password: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<u16>,
    pub dav_url: String,
    pub created_account: bool,
}

impl std::fmt::Debug for NewMoveMailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The password never into a log line.
        f.debug_struct("NewMoveMailbox")
            .field("account_id", &self.account_id)
            .field("old_address", &self.old_address)
            .field("login", &self.login)
            .field("imap_host", &self.imap_host)
            .finish_non_exhaustive()
    }
}

/// How a turn of the worker on one mailbox ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveTurn {
    /// Its time was up; it goes back in the queue.
    Continue,
    /// The round is complete: everything the old mailbox had is here.
    RoundDone,
    /// Stopped until the admin does something.
    Paused { code: String, detail: String },
}

/// What a change to a move's settings may say.
#[derive(Debug, Clone, Default)]
pub struct MoveSettings {
    pub parallel: Option<i64>,
    pub sync_minutes: Option<i64>,
}

const MOVE_COLUMNS: &str = "id, kind, domain, imap_host, imap_port, dav_mode, dav_host, dav_url, contacts, calendars, \
                            parallel, sync_minutes, state, created_at, finish_requested_at, finished_at";

fn move_from_row(row: &Row<'_>) -> rusqlite::Result<Move> {
    Ok(Move {
        id: row.get(0)?,
        kind: MoveKind::parse(&row.get::<_, String>(1)?).unwrap_or(MoveKind::Domain),
        domain: row.get(2)?,
        imap_host: row.get(3)?,
        imap_port: row.get::<_, i64>(4)? as u16,
        dav_mode: DavMode::parse(&row.get::<_, String>(5)?).unwrap_or(DavMode::Auto),
        dav_host: row.get(6)?,
        dav_url: row.get(7)?,
        contacts: row.get(8)?,
        calendars: row.get(9)?,
        parallel: row.get(10)?,
        sync_minutes: row.get(11)?,
        state: MoveState::parse(&row.get::<_, String>(12)?),
        created_at: row.get(13)?,
        finish_requested_at: row.get(14)?,
        finished_at: row.get(15)?,
        summary: MoveSummary::default(),
    })
}

const MAILBOX_COLUMNS: &str = "m.id, m.move_id, m.account_id, a.login, a.display_name, a.quota_bytes, a.used_bytes, \
     m.old_address, m.login, m.imap_host, m.imap_port, m.dav_url, m.created_account, m.password_sealed IS NOT NULL, \
     m.state, m.final_round, m.error, m.error_detail, m.folders_done, m.folders_total, m.messages_done, \
     m.messages_total, m.messages_skipped, m.bytes_done, m.source_bytes, m.contacts_done + m.dav_contacts, \
     m.events_done + m.dav_events, m.dav_error, \
     m.dav_found, m.rounds, m.created_at, m.last_run_at, m.last_synced_at, m.next_sync_at, m.finished_at";
const MAILBOX_FROM: &str = "move_mailboxes m JOIN accounts a ON a.id = m.account_id";

fn mailbox_from_row(row: &Row<'_>) -> rusqlite::Result<MoveMailbox> {
    Ok(MoveMailbox {
        id: row.get(0)?,
        move_id: row.get(1)?,
        account_id: row.get(2)?,
        address: row.get(3)?,
        display_name: row.get(4)?,
        quota_bytes: row.get(5)?,
        used_bytes: row.get(6)?,
        old_address: row.get(7)?,
        login: row.get(8)?,
        imap_host: row.get(9)?,
        imap_port: row.get::<_, i64>(10)? as u16,
        dav_url: row.get(11)?,
        created_account: row.get(12)?,
        has_password: row.get(13)?,
        state: MoveMailboxState::parse(&row.get::<_, String>(14)?),
        final_round: row.get(15)?,
        error: row.get(16)?,
        error_detail: row.get(17)?,
        progress: MigrationProgress {
            folders_done: row.get(18)?,
            folders_total: row.get(19)?,
            messages_done: row.get(20)?,
            messages_total: row.get(21)?,
            messages_skipped: row.get(22)?,
            bytes_done: row.get(23)?,
        },
        source_bytes: row.get(24)?,
        contacts_done: row.get(25)?,
        events_done: row.get(26)?,
        dav_error: row.get(27)?,
        dav_found: row.get(28)?,
        rounds: row.get(29)?,
        created_at: row.get(30)?,
        last_run_at: row.get(31)?,
        last_synced_at: row.get(32)?,
        next_sync_at: row.get(33)?,
        finished_at: row.get(34)?,
    })
}

fn shorten(value: &str) -> String {
    let value = value.trim();
    match value.char_indices().nth(300) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => value.to_owned(),
    }
}

fn load_move(conn: &Connection, id: i64) -> Result<Move> {
    let mut found = conn
        .query_row(&format!("SELECT {MOVE_COLUMNS} FROM moves WHERE id = ?1"), [id], move_from_row)
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("move {id}")))?;
    found.summary = summary(conn, id)?;
    Ok(found)
}

fn summary(conn: &Connection, id: i64) -> Result<MoveSummary> {
    Ok(conn.query_row(
        "SELECT count(*), coalesce(sum(state = 'queued'), 0), coalesce(sum(state = 'running'), 0),
                coalesce(sum(state = 'paused'), 0), coalesce(sum(state = 'synced'), 0),
                coalesce(sum(state = 'done'), 0), coalesce(sum(messages_done), 0),
                coalesce(sum(messages_total), 0), coalesce(sum(messages_skipped), 0), coalesce(sum(bytes_done), 0),
                coalesce(sum(contacts_done + dav_contacts), 0), coalesce(sum(events_done + dav_events), 0)
         FROM move_mailboxes WHERE move_id = ?1",
        [id],
        |row| {
            Ok(MoveSummary {
                mailboxes: row.get(0)?,
                queued: row.get(1)?,
                running: row.get(2)?,
                paused: row.get(3)?,
                synced: row.get(4)?,
                done: row.get(5)?,
                messages_done: row.get(6)?,
                messages_total: row.get(7)?,
                messages_skipped: row.get(8)?,
                bytes_done: row.get(9)?,
                contacts_done: row.get(10)?,
                events_done: row.get(11)?,
            })
        },
    )?)
}

fn load_mailbox(conn: &Connection, move_id: Option<i64>, id: i64) -> Result<MoveMailbox> {
    conn.query_row(
        &format!("SELECT {MAILBOX_COLUMNS} FROM {MAILBOX_FROM} WHERE m.id = ?1 AND (?2 IS NULL OR m.move_id = ?2)"),
        params![id, move_id],
        mailbox_from_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("mailbox {id} of the move")))
}

fn rule(code: &'static str, message: impl Into<String>) -> StoreError {
    StoreError::Rule { code, message: message.into() }
}

/// A server address as the admin typed it: `https://` only, no login inside, not too long.
fn check_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(String::new());
    }
    let lower = url.to_ascii_lowercase();
    if url.len() > MAX_URL || !lower.starts_with("https://") || url.contains(char::is_whitespace) {
        return Err(StoreError::Invalid(format!("'{url}' is not an https address")));
    }
    let host = lower["https://".len()..].split(['/', '?', '#']).next().unwrap_or_default();
    if host.contains('@') {
        return Err(StoreError::Invalid("the address may not carry a login".into()));
    }
    let name = host.rsplit_once(':').map_or(host, |(name, port)| if port.parse::<u16>().is_ok() { name } else { host });
    check_host(name)?;
    Ok(url.to_owned())
}

fn check_settings(parallel: i64, sync_minutes: i64) -> Result<()> {
    if !(1..=MAX_MOVE_PARALLEL).contains(&parallel) {
        return Err(StoreError::Invalid(format!("between 1 and {MAX_MOVE_PARALLEL} mailboxes at once")));
    }
    if !(MIN_MOVE_SYNC_MINUTES..=MAX_MOVE_SYNC_MINUTES).contains(&sync_minutes) {
        return Err(StoreError::Invalid(format!(
            "a round every {MIN_MOVE_SYNC_MINUTES} to {MAX_MOVE_SYNC_MINUTES} minutes"
        )));
    }
    Ok(())
}

/// Checks and normalizes one new mailbox of a move.
fn check_mailbox(new: &mut NewMoveMailbox) -> Result<()> {
    let (local, domain) = normalize_address(&new.old_address)
        .map_err(|_| StoreError::Invalid(format!("'{}' is not a valid email address", new.old_address)))?;
    new.old_address = format!("{local}@{domain}");
    new.login = new.login.trim().to_owned();
    if new.login.is_empty() {
        new.login = new.old_address.clone();
    }
    if new.login.chars().count() > MAX_LOGIN || new.login.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!("the login for {} is not usable", new.old_address)));
    }
    if new.password.is_empty() || new.password.len() > MAX_PASSWORD || new.password.contains(['\r', '\n', '\0']) {
        return Err(StoreError::Invalid(format!("the password for {} is missing or not usable", new.old_address)));
    }
    new.imap_host = match new.imap_host.take().map(|host| host.trim().to_owned()).filter(|host| !host.is_empty()) {
        Some(host) => Some(check_host(&host)?),
        None => None,
    };
    if new.imap_port == Some(0) {
        return Err(StoreError::Invalid("the port of the old server is missing".into()));
    }
    new.dav_url = check_url(&new.dav_url)?;
    Ok(())
}

/// Adds mailboxes to a move inside a write: each account at most once in all moves that are not
/// done, so two workers never fill one mailbox.
fn insert_mailboxes(tx: &Connection, move_id: i64, domain: &str, mailboxes: Vec<NewMoveMailbox>) -> Result<usize> {
    let held: i64 = tx.query_row("SELECT count(*) FROM move_mailboxes WHERE move_id = ?1", [move_id], |r| r.get(0))?;
    if held as usize + mailboxes.len() > MAX_MOVE_MAILBOXES {
        return Err(rule("moveTooMany", format!("at most {MAX_MOVE_MAILBOXES} mailboxes in one move")));
    }
    let at = now();
    let mut added = 0;
    for mut new in mailboxes {
        check_mailbox(&mut new)?;
        let account: Option<(String, Option<i64>, i64, i64)> = tx
            .query_row(
                "SELECT login, deleted_at, imap_enabled, jmap_enabled FROM accounts WHERE id = ?1",
                [new.account_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((login, deleted_at, imap, jmap)) = account else {
            return Err(StoreError::NotFound(format!("account {}", new.account_id)));
        };
        if deleted_at.is_some() || (imap == 0 && jmap == 0) {
            return Err(rule("noMailbox", format!("{login} has no mailbox to move into")));
        }
        if !login.ends_with(&format!("@{domain}")) {
            return Err(rule("moveOtherDomain", format!("{login} is not on {domain}")));
        }
        let busy: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM move_mailboxes WHERE account_id = ?1 AND state != 'done')",
            [new.account_id],
            |row| row.get(0),
        )?;
        if busy {
            return Err(rule("moveMailboxBusy", format!("{login} is being moved already")));
        }
        let sealed = seal(tx, &new.password)?;
        tx.execute(
            "INSERT INTO move_mailboxes (move_id, account_id, old_address, login, password_sealed, imap_host,
                 imap_port, dav_url, created_account, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                move_id,
                new.account_id,
                new.old_address,
                new.login,
                sealed,
                new.imap_host.unwrap_or_default(),
                i64::from(new.imap_port.unwrap_or(0)),
                new.dav_url,
                new.created_account,
                at
            ],
        )?;
        added += 1;
    }
    Ok(added)
}

/// A finishing move whose mailboxes are all done is done.
fn settle(tx: &Connection, move_id: i64) -> Result<()> {
    tx.execute(
        "UPDATE moves SET state = 'done', finished_at = ?2
         WHERE id = ?1 AND state = 'finishing'
           AND NOT EXISTS (SELECT 1 FROM move_mailboxes WHERE move_id = ?1 AND state != 'done')",
        params![move_id, now()],
    )?;
    Ok(())
}

impl Store {
    /// Starts a move with its mailboxes; they are queued at once.
    pub async fn create_move(&self, new: NewMove, mailboxes: Vec<NewMoveMailbox>) -> Result<Move> {
        let imap_host = check_host(&new.imap_host)?;
        if new.imap_port == 0 {
            return Err(StoreError::Invalid("the port of the old server is missing".into()));
        }
        check_settings(new.parallel, new.sync_minutes)?;
        let dav_host = match new.dav_host.trim() {
            "" => String::new(),
            host => check_host(host)?,
        };
        let dav_url = check_url(&new.dav_url)?;
        if new.dav_mode == DavMode::Custom && dav_url.is_empty() {
            return Err(StoreError::Invalid("the CalDAV/CardDAV address is missing".into()));
        }
        if mailboxes.is_empty() {
            return Err(StoreError::Invalid("a move needs at least one mailbox".into()));
        }
        if new.kind == MoveKind::Mailbox && mailboxes.len() != 1 {
            return Err(StoreError::Invalid("a single move takes exactly one mailbox".into()));
        }
        let domain = crate::address::normalize_domain(&new.domain)?;
        self.write(move |tx| {
            let kind: Option<String> =
                tx.query_row("SELECT kind FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?;
            match kind.as_deref() {
                None => return Err(StoreError::NotFound(format!("domain {domain}"))),
                Some("mail") => {}
                Some(_) => return Err(rule("domainMaskedOnly", format!("{domain} takes masked addresses only"))),
            }
            let open: i64 = tx.query_row("SELECT count(*) FROM moves WHERE state != 'done'", [], |row| row.get(0))?;
            if open as usize >= MAX_OPEN_MOVES {
                return Err(rule("moveLimit", format!("at most {MAX_OPEN_MOVES} moves at once")));
            }
            tx.execute(
                "INSERT INTO moves (kind, domain, imap_host, imap_port, dav_mode, dav_host, dav_url, contacts,
                     calendars, parallel, sync_minutes, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    new.kind.as_str(),
                    domain,
                    imap_host,
                    i64::from(new.imap_port),
                    new.dav_mode.as_str(),
                    dav_host,
                    dav_url,
                    new.contacts,
                    new.calendars,
                    new.parallel,
                    new.sync_minutes,
                    new.created_by,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            insert_mailboxes(tx, id, &domain, mailboxes)?;
            load_move(tx, id)
        })
        .await
    }

    /// More mailboxes for a move that is not finishing or done.
    pub async fn add_move_mailboxes(&self, move_id: i64, mailboxes: Vec<NewMoveMailbox>) -> Result<Move> {
        self.write(move |tx| {
            let found = load_move(tx, move_id)?;
            if matches!(found.state, MoveState::Finishing | MoveState::Done) {
                return Err(rule("moveFinished", "this move is finished"));
            }
            if found.kind == MoveKind::Mailbox {
                return Err(rule("moveSingle", "a single move takes exactly one mailbox"));
            }
            insert_mailboxes(tx, move_id, &found.domain, mailboxes)?;
            load_move(tx, move_id)
        })
        .await
    }

    /// Every move, newest first.
    pub async fn moves(&self) -> Result<Vec<Move>> {
        self.read(|conn| {
            let ids: Vec<i64> = conn
                .prepare("SELECT id FROM moves ORDER BY id DESC")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids.into_iter().map(|id| load_move(conn, id)).collect()
        })
        .await
    }

    pub async fn move_by_id(&self, id: i64) -> Result<Option<Move>> {
        self.read(move |conn| match load_move(conn, id) {
            Ok(found) => Ok(Some(found)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        })
        .await
    }

    /// The mailboxes of a move, by their address here.
    pub async fn move_mailboxes(&self, move_id: i64) -> Result<Vec<MoveMailbox>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {MAILBOX_COLUMNS} FROM {MAILBOX_FROM} WHERE m.move_id = ?1 ORDER BY a.login"
            ))?;
            let rows = stmt.query_map([move_id], mailbox_from_row)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// One mailbox of a move; with `move_id`, only when it belongs to that move.
    pub async fn move_mailbox(&self, move_id: Option<i64>, id: i64) -> Result<Option<MoveMailbox>> {
        self.read(move |conn| match load_mailbox(conn, move_id, id) {
            Ok(found) => Ok(Some(found)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        })
        .await
    }

    /// The password for an old mailbox, for the worker; `None` once it is wiped.
    pub async fn move_mailbox_password(&self, id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            let sealed: Option<Option<Vec<u8>>> = conn
                .query_row("SELECT password_sealed FROM move_mailboxes WHERE id = ?1", [id], |row| row.get(0))
                .optional()?;
            sealed.flatten().map(|sealed| unseal(conn, &sealed)).transpose()
        })
        .await
    }

    pub async fn change_move_settings(&self, id: i64, change: MoveSettings) -> Result<Move> {
        self.write(move |tx| {
            let found = load_move(tx, id)?;
            let parallel = change.parallel.unwrap_or(found.parallel);
            let sync_minutes = change.sync_minutes.unwrap_or(found.sync_minutes);
            check_settings(parallel, sync_minutes)?;
            tx.execute(
                "UPDATE moves SET parallel = ?2, sync_minutes = ?3 WHERE id = ?1",
                params![id, parallel, sync_minutes],
            )?;
            // Waiting mailboxes take the new rhythm from their last round.
            tx.execute(
                "UPDATE move_mailboxes SET next_sync_at = last_synced_at + ?2 * 60
                 WHERE move_id = ?1 AND state = 'synced' AND last_synced_at IS NOT NULL",
                params![id, sync_minutes],
            )?;
            load_move(tx, id)
        })
        .await
    }

    /// Stops a move until the admin goes on with it. Running mailboxes stop after their portion.
    pub async fn pause_move(&self, id: i64) -> Result<Move> {
        self.write(move |tx| {
            let changed = tx.execute("UPDATE moves SET state = 'paused' WHERE id = ?1 AND state = 'active'", [id])?;
            let found = load_move(tx, id)?;
            if changed == 0 && found.state != MoveState::Paused {
                return Err(rule("moveNotActive", "this move is not copying"));
            }
            Ok(found)
        })
        .await
    }

    pub async fn resume_move(&self, id: i64) -> Result<Move> {
        self.write(move |tx| {
            let changed = tx.execute("UPDATE moves SET state = 'active' WHERE id = ?1 AND state = 'paused'", [id])?;
            let found = load_move(tx, id)?;
            if changed == 0 && found.state != MoveState::Active {
                return Err(rule("moveNotPaused", "this move is not paused"));
            }
            Ok(found)
        })
        .await
    }

    /// Queues one mailbox again: after a pause it goes on where it stopped, when it is up to date
    /// a round starts now. A new login or password replaces the stored ones.
    pub async fn retry_move_mailbox(
        &self,
        move_id: i64,
        id: i64,
        login: Option<String>,
        password: Option<String>,
    ) -> Result<MoveMailbox> {
        self.write(move |tx| {
            let mailbox = load_mailbox(tx, Some(move_id), id)?;
            match mailbox.state {
                MoveMailboxState::Queued | MoveMailboxState::Running => {
                    return Err(rule("moveRunning", "this mailbox is being copied"));
                }
                MoveMailboxState::Done => return Err(rule("moveFinished", "this mailbox is finished")),
                MoveMailboxState::Paused | MoveMailboxState::Synced => {}
            }
            if let Some(login) = login.map(|login| login.trim().to_owned()).filter(|login| !login.is_empty()) {
                if login.chars().count() > MAX_LOGIN || login.chars().any(char::is_control) {
                    return Err(StoreError::Invalid("this login is not usable".into()));
                }
                tx.execute("UPDATE move_mailboxes SET login = ?2 WHERE id = ?1", params![id, login])?;
            }
            if let Some(password) = password.filter(|password| !password.is_empty()) {
                if password.len() > MAX_PASSWORD || password.contains(['\r', '\n', '\0']) {
                    return Err(StoreError::Invalid("this password is not usable".into()));
                }
                let sealed = seal(tx, &password)?;
                tx.execute("UPDATE move_mailboxes SET password_sealed = ?2 WHERE id = ?1", params![id, sealed])?;
            }
            tx.execute(
                "UPDATE move_mailboxes SET state = 'queued', error = '', error_detail = '' WHERE id = ?1",
                [id],
            )?;
            load_mailbox(tx, Some(move_id), id)
        })
        .await
    }

    /// Stops one mailbox until the admin goes on with it.
    pub async fn pause_move_mailbox(&self, move_id: i64, id: i64) -> Result<MoveMailbox> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE move_mailboxes SET state = 'paused', error = 'stopped', error_detail = ''
                 WHERE id = ?1 AND move_id = ?2 AND state IN ('queued', 'running', 'synced')",
                params![id, move_id],
            )?;
            let mailbox = load_mailbox(tx, Some(move_id), id)?;
            if changed == 0 && mailbox.state != MoveMailboxState::Paused {
                return Err(rule("moveNotRunning", "this mailbox is not being copied"));
            }
            Ok(mailbox)
        })
        .await
    }

    /// Takes one mailbox out of a move, with its password. What was copied stays.
    pub async fn remove_move_mailbox(&self, move_id: i64, id: i64) -> Result<()> {
        self.write(move |tx| {
            let changed =
                tx.execute("DELETE FROM move_mailboxes WHERE id = ?1 AND move_id = ?2", params![id, move_id])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("mailbox {id} of the move")));
            }
            settle(tx, move_id)?;
            Ok(())
        })
        .await
    }

    /// The admin finishes a move, once the MX records point here: every mailbox gets one last
    /// round, after which its password is wiped. `skip_last_round` wipes them at once instead --
    /// for an old server that is gone already, or a last round that cannot succeed.
    pub async fn finish_move(&self, id: i64, skip_last_round: bool) -> Result<Move> {
        let at = now();
        self.write(move |tx| {
            let found = load_move(tx, id)?;
            match found.state {
                MoveState::Done => return Err(rule("moveFinished", "this move is finished")),
                MoveState::Finishing if !skip_last_round => {
                    return Err(rule("moveFinishing", "the last round is running"));
                }
                _ => {}
            }
            tx.execute(
                "UPDATE moves SET state = 'finishing', finish_requested_at = coalesce(finish_requested_at, ?2)
                 WHERE id = ?1",
                params![id, at],
            )?;
            if skip_last_round {
                tx.execute(
                    "UPDATE move_mailboxes SET state = 'done', password_sealed = NULL, final_round = 1,
                         finished_at = ?2, next_sync_at = NULL
                     WHERE move_id = ?1 AND state != 'done'",
                    params![id, at],
                )?;
            } else {
                tx.execute("UPDATE move_mailboxes SET final_round = 1 WHERE move_id = ?1 AND state != 'done'", [id])?;
                tx.execute(
                    "UPDATE move_mailboxes SET state = 'queued', error = '', error_detail = ''
                     WHERE move_id = ?1 AND state IN ('synced', 'paused')",
                    [id],
                )?;
            }
            settle(tx, id)?;
            load_move(tx, id)
        })
        .await
    }

    /// Forgets a move with every password in it. The mailboxes and what was copied stay.
    pub async fn delete_move(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            if tx.execute("DELETE FROM moves WHERE id = ?1", [id])? == 0 {
                return Err(StoreError::NotFound(format!("move {id}")));
            }
            Ok(())
        })
        .await
    }

    /// Takes the next queued mailbox of a move that is copying and has a free slot, the one that
    /// waited longest, and marks it running.
    pub async fn take_move_mailbox(&self) -> Result<Option<MoveMailbox>> {
        let at = now();
        self.write(move |tx| {
            let id: Option<i64> = tx
                .query_row(
                    "SELECT m.id FROM move_mailboxes m JOIN moves v ON v.id = m.move_id
                     JOIN accounts a ON a.id = m.account_id
                     WHERE m.state = 'queued' AND v.state IN ('active', 'finishing') AND a.deleted_at IS NULL
                       AND (SELECT count(*) FROM move_mailboxes r WHERE r.move_id = m.move_id AND r.state = 'running')
                           < v.parallel
                     ORDER BY m.last_run_at IS NOT NULL, m.last_run_at, m.id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(id) = id else { return Ok(None) };
            tx.execute("UPDATE move_mailboxes SET state = 'running', last_run_at = ?2 WHERE id = ?1", params![id, at])?;
            Ok(Some(load_mailbox(tx, None, id)?))
        })
        .await
    }

    /// Up-to-date mailboxes whose next round is due go back in the queue.
    pub async fn queue_due_move_mailboxes(&self) -> Result<usize> {
        let at = now();
        self.write(move |tx| {
            Ok(tx.execute(
                "UPDATE move_mailboxes SET state = 'queued'
                 WHERE state = 'synced' AND next_sync_at <= ?1
                   AND move_id IN (SELECT id FROM moves WHERE state = 'active')",
                [at],
            )?)
        })
        .await
    }

    /// Mailboxes that were running when the server stopped go back in the queue.
    pub async fn requeue_running_moves(&self) -> Result<usize> {
        self.write(|tx| Ok(tx.execute("UPDATE move_mailboxes SET state = 'queued' WHERE state = 'running'", [])?)).await
    }

    /// Writes down how far a running mailbox got. `false` when it should stop: it or its move
    /// was paused, removed or finished without a last round.
    pub async fn note_move_progress(&self, id: i64, progress: MigrationProgress) -> Result<bool> {
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE move_mailboxes SET folders_done = ?2, folders_total = ?3, messages_done = ?4,
                     messages_total = ?5, messages_skipped = ?6, bytes_done = ?7
                 WHERE id = ?1 AND state = 'running'
                   AND move_id IN (SELECT id FROM moves WHERE state IN ('active', 'finishing'))",
                params![
                    id,
                    progress.folders_done,
                    progress.folders_total,
                    progress.messages_done,
                    progress.messages_total,
                    progress.messages_skipped,
                    progress.bytes_done,
                ],
            )?;
            Ok(changed == 1)
        })
        .await
    }

    /// What the old mailbox holds altogether, as its server said.
    pub async fn note_move_source_size(&self, id: i64, bytes: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("UPDATE move_mailboxes SET source_bytes = ?2 WHERE id = ?1", params![id, bytes.max(0)])?;
            Ok(())
        })
        .await
    }

    /// How contacts and calendars went: how many the old provider had (they replace the counts of
    /// the round before, which were the same entries), an error code or empty, and the
    /// collections found, when they were looked for.
    pub async fn note_move_dav(
        &self,
        id: i64,
        contacts: i64,
        events: i64,
        error: &str,
        found: Option<String>,
    ) -> Result<()> {
        let error = shorten(error);
        self.write(move |tx| {
            tx.execute(
                "UPDATE move_mailboxes SET dav_contacts = ?2, dav_events = ?3, dav_error = ?4,
                     dav_found = coalesce(?5, dav_found)
                 WHERE id = ?1",
                params![id, contacts, events, error, found],
            )?;
            Ok(())
        })
        .await
    }

    /// Contacts and calendar entries a turn found in IMAP folders, added to the counts.
    pub async fn count_move_objects(&self, id: i64, contacts: i64, events: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE move_mailboxes SET contacts_done = contacts_done + ?2, events_done = events_done + ?3
                 WHERE id = ?1",
                params![id, contacts.max(0), events.max(0)],
            )?;
            Ok(())
        })
        .await
    }

    /// Hands a mailbox back after a turn. Only a mailbox that is still running changes. A round
    /// that ended while the mailbox was in its last round (`last_round` says whether the turn
    /// started as one) finishes it and wipes its password; a round that ended just before the
    /// admin finished the move is followed by the last one.
    pub async fn finish_move_turn(&self, id: i64, turn: MoveTurn, last_round: bool) -> Result<()> {
        let at = now();
        self.write(move |tx| {
            let row: Option<(i64, bool, i64)> = tx
                .query_row(
                    "SELECT m.move_id, m.final_round, v.sync_minutes FROM move_mailboxes m
                     JOIN moves v ON v.id = m.move_id WHERE m.id = ?1 AND m.state = 'running'",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((move_id, final_round, sync_minutes)) = row else { return Ok(()) };
            match turn {
                MoveTurn::Continue => {
                    tx.execute("UPDATE move_mailboxes SET state = 'queued' WHERE id = ?1", [id])?;
                }
                MoveTurn::Paused { code, detail } => {
                    tx.execute(
                        "UPDATE move_mailboxes SET state = 'paused', error = ?2, error_detail = ?3 WHERE id = ?1",
                        params![id, code, shorten(&detail)],
                    )?;
                }
                MoveTurn::RoundDone if final_round && last_round => {
                    tx.execute(
                        "UPDATE move_mailboxes SET state = 'done', password_sealed = NULL, error = '',
                             error_detail = '', rounds = rounds + 1, last_synced_at = ?2, finished_at = ?2,
                             next_sync_at = NULL, folders_done = folders_total, messages_total = messages_done
                         WHERE id = ?1",
                        params![id, at],
                    )?;
                }
                MoveTurn::RoundDone if final_round => {
                    tx.execute(
                        "UPDATE move_mailboxes SET state = 'queued', rounds = rounds + 1, last_synced_at = ?2
                         WHERE id = ?1",
                        params![id, at],
                    )?;
                }
                MoveTurn::RoundDone => {
                    tx.execute(
                        "UPDATE move_mailboxes SET state = 'synced', error = '', error_detail = '',
                             rounds = rounds + 1, last_synced_at = ?2, next_sync_at = ?2 + ?3 * 60,
                             folders_done = folders_total, messages_total = messages_done
                         WHERE id = ?1",
                        params![id, at, sync_minutes],
                    )?;
                }
            }
            settle(tx, move_id)?;
            Ok(())
        })
        .await
    }
}

impl Store {
    /// Brings calendar entries or cards a move found (at the old provider, in an IMAP folder or in
    /// a file) into the account's own collection of the same name, made when there is none: one
    /// round after the other merges into the same one, by the entries' UIDs, so nothing comes
    /// twice. What the cutting left out is in the report too.
    pub async fn move_dav_import(
        &self,
        account_id: i64,
        kind: DavKind,
        new: NewImportCollection,
        default: NewDavCollection,
        split: Split,
    ) -> Result<(DavCollection, DavImportReport)> {
        let wanted = if new.name.trim().is_empty() { default.display_name.clone() } else { new.name.trim().to_owned() };
        let existing = self
            .dav_collections(account_id, kind, default.clone())
            .await?
            .into_iter()
            .find(|c| c.account_id == account_id && c.display_name.trim().eq_ignore_ascii_case(&wanted));
        let collection = match existing {
            Some(collection) => collection,
            None => self.dav_create_import_collection(account_id, kind, new, default).await?,
        };
        let mut report = self.dav_import(account_id, collection.id, split.objects, DavImportMode::Merge).await?;
        for problem in split.problems {
            report.total += 1;
            report.problem(problem.item, problem.reason);
        }
        Ok((collection, report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    async fn person(store: &Store, address: &str) -> i64 {
        let new = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(new).await.unwrap().id
    }

    fn new_move(kind: MoveKind) -> NewMove {
        NewMove {
            kind,
            domain: "example.org".into(),
            imap_host: "imap.example.net".into(),
            imap_port: 993,
            dav_mode: DavMode::Auto,
            dav_host: String::new(),
            dav_url: String::new(),
            contacts: true,
            calendars: true,
            parallel: 1,
            sync_minutes: 30,
            created_by: None,
        }
    }

    fn mailbox(account_id: i64, address: &str) -> NewMoveMailbox {
        NewMoveMailbox {
            account_id,
            old_address: address.into(),
            login: String::new(),
            password: "altes-passwort".into(),
            imap_host: None,
            imap_port: None,
            dav_url: String::new(),
            created_account: true,
        }
    }

    fn code(err: StoreError) -> &'static str {
        match err {
            StoreError::Rule { code, .. } => code,
            other => panic!("not a rule: {other:?}"),
        }
    }

    #[test]
    fn debug_leaves_the_password_out() {
        let shown = format!("{:?}", mailbox(1, "mini@example.org"));
        assert!(shown.contains("mini@example.org") && !shown.contains("altes-passwort"), "{shown}");
    }

    #[test]
    fn server_addresses_are_checked() {
        assert_eq!(check_url("https://dav.example.net/SOGo/dav/").unwrap(), "https://dav.example.net/SOGo/dav/");
        assert_eq!(check_url("https://dav.example.net:8443/x").unwrap(), "https://dav.example.net:8443/x");
        assert!(check_url("http://dav.example.net/").is_err());
        assert!(check_url("https://user:pw@dav.example.net/").is_err());
        assert!(check_url("https://192.168.1.2/").is_err());
        assert_eq!(check_url("  ").unwrap(), "");
        assert_eq!(DavMode::parse("mailcow"), Some(DavMode::Sogo));
        assert_eq!(DavMode::parse("bogus"), None);
    }

    #[tokio::test]
    async fn a_domain_move_keeps_up_until_it_is_finished() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let nyu = person(&store, "nyu@example.org").await;

        // The old addresses may be on this domain: that is what a domain move is.
        let created = store
            .create_move(
                new_move(MoveKind::Domain),
                vec![mailbox(mini, "Mini@Example.org"), mailbox(nyu, "nyu@example.org")],
            )
            .await
            .unwrap();
        assert_eq!((created.state, created.summary.mailboxes, created.summary.queued), (MoveState::Active, 2, 2));
        let boxes = store.move_mailboxes(created.id).await.unwrap();
        assert_eq!(boxes[0].old_address, "mini@example.org");
        assert_eq!(boxes[0].login, "mini@example.org", "the login is the old address unless given");
        let json = serde_json::to_string(&boxes).unwrap();
        assert!(!json.contains("altes-passwort"), "{json}");
        assert_eq!(store.move_mailbox_password(boxes[0].id).await.unwrap().as_deref(), Some("altes-passwort"));

        // One mailbox in one move at a time.
        let again = store.create_move(new_move(MoveKind::Mailbox), vec![mailbox(mini, "mini@example.net")]).await;
        assert_eq!(code(again.unwrap_err()), "moveMailboxBusy");

        // One at once, as the move says.
        let first = store.take_move_mailbox().await.unwrap().unwrap();
        assert!(store.take_move_mailbox().await.unwrap().is_none(), "the move's slot is taken");
        let progress =
            MigrationProgress { folders_total: 2, messages_total: 5, messages_done: 5, ..Default::default() };
        assert!(store.note_move_progress(first.id, progress).await.unwrap());
        store.finish_move_turn(first.id, MoveTurn::RoundDone, false).await.unwrap();
        let synced = store.move_mailbox(Some(created.id), first.id).await.unwrap().unwrap();
        assert_eq!((synced.state, synced.rounds, synced.progress.folders_done), (MoveMailboxState::Synced, 1, 2));
        assert!(synced.next_sync_at.unwrap() >= synced.last_synced_at.unwrap() + 30 * 60);
        assert_eq!(store.queue_due_move_mailboxes().await.unwrap(), 0, "not due yet");

        // The second one pauses on a refused password; retrying takes a new one.
        let second = store.take_move_mailbox().await.unwrap().unwrap();
        assert_ne!(second.id, first.id);
        let refused = MoveTurn::Paused { code: "loginRefused".into(), detail: "NO".into() };
        store.finish_move_turn(second.id, refused, false).await.unwrap();
        store.retry_move_mailbox(created.id, second.id, None, Some("neu".into())).await.unwrap();
        assert_eq!(store.move_mailbox_password(second.id).await.unwrap().as_deref(), Some("neu"));

        // Pausing the move stops the running one at its next note and keeps the queue waiting.
        let second = store.take_move_mailbox().await.unwrap().unwrap();
        store.pause_move(created.id).await.unwrap();
        assert!(!store.note_move_progress(second.id, progress).await.unwrap());
        store.finish_move_turn(second.id, MoveTurn::Continue, false).await.unwrap();
        assert!(store.take_move_mailbox().await.unwrap().is_none());
        store.resume_move(created.id).await.unwrap();

        // A restart puts running mailboxes back.
        let second = store.take_move_mailbox().await.unwrap().unwrap();
        assert_eq!(store.requeue_running_moves().await.unwrap(), 1);

        // Finishing: everyone gets a last round; the running round of before is followed by one.
        let taken = store.take_move_mailbox().await.unwrap().unwrap();
        assert_eq!(taken.id, second.id);
        let finishing = store.finish_move(created.id, false).await.unwrap();
        assert_eq!(finishing.state, MoveState::Finishing);
        assert_eq!(code(store.finish_move(created.id, false).await.unwrap_err()), "moveFinishing");
        store.finish_move_turn(second.id, MoveTurn::RoundDone, false).await.unwrap();
        let after = store.move_mailbox(None, second.id).await.unwrap().unwrap();
        assert_eq!((after.state, after.has_password), (MoveMailboxState::Queued, true));
        // The first, synced before, is queued for its last round too.
        let mut ended = 0;
        while let Some(taken) = store.take_move_mailbox().await.unwrap() {
            assert!(taken.final_round);
            store.finish_move_turn(taken.id, MoveTurn::RoundDone, true).await.unwrap();
            ended += 1;
        }
        assert_eq!(ended, 2);
        let done = store.move_by_id(created.id).await.unwrap().unwrap();
        assert_eq!((done.state, done.summary.done), (MoveState::Done, 2));
        for mailbox in store.move_mailboxes(created.id).await.unwrap() {
            assert!(!mailbox.has_password, "passwords are wiped");
            assert_eq!(store.move_mailbox_password(mailbox.id).await.unwrap(), None);
        }
        // A finished mailbox may be moved again later.
        store.create_move(new_move(MoveKind::Mailbox), vec![mailbox(mini, "mini@example.net")]).await.unwrap();
    }

    #[tokio::test]
    async fn finishing_without_a_last_round_wipes_at_once() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let created =
            store.create_move(new_move(MoveKind::Mailbox), vec![mailbox(mini, "mini@example.net")]).await.unwrap();
        let running = store.take_move_mailbox().await.unwrap().unwrap();
        let done = store.finish_move(created.id, true).await.unwrap();
        assert_eq!(done.state, MoveState::Done);
        assert_eq!(store.move_mailbox_password(running.id).await.unwrap(), None);
        // The worker that was running stops and changes nothing.
        assert!(!store.note_move_progress(running.id, MigrationProgress::default()).await.unwrap());
        store.finish_move_turn(running.id, MoveTurn::Continue, false).await.unwrap();
        assert_eq!(store.move_mailbox(None, running.id).await.unwrap().unwrap().state, MoveMailboxState::Done);
    }

    #[tokio::test]
    async fn moves_are_checked() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("example.com").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let other = person(&store, "leni@example.com").await;
        let refused = store.create_move(new_move(MoveKind::Domain), vec![mailbox(other, "leni@example.com")]).await;
        assert_eq!(code(refused.unwrap_err()), "moveOtherDomain");
        let private = NewMove { imap_host: "10.0.0.1".into(), ..new_move(MoveKind::Domain) };
        assert!(matches!(
            store.create_move(private, vec![mailbox(mini, "a@example.org")]).await,
            Err(StoreError::Invalid(_))
        ));
        let wild = NewMove { parallel: 50, ..new_move(MoveKind::Domain) };
        assert!(matches!(
            store.create_move(wild, vec![mailbox(mini, "a@example.org")]).await,
            Err(StoreError::Invalid(_))
        ));
        let no_password = NewMoveMailbox { password: String::new(), ..mailbox(mini, "a@example.org") };
        assert!(store.create_move(new_move(MoveKind::Domain), vec![no_password]).await.is_err());
        let two = store
            .create_move(
                new_move(MoveKind::Mailbox),
                vec![mailbox(mini, "a@example.org"), mailbox(other, "b@example.org")],
            )
            .await;
        assert!(matches!(two, Err(StoreError::Invalid(_))));
        let custom = NewMove { dav_mode: DavMode::Custom, ..new_move(MoveKind::Domain) };
        assert!(store.create_move(custom, vec![mailbox(mini, "a@example.org")]).await.is_err());
        let unknown = NewMove { domain: "example.net".into(), ..new_move(MoveKind::Domain) };
        assert!(matches!(
            store.create_move(unknown, vec![mailbox(mini, "a@example.org")]).await,
            Err(StoreError::NotFound(_))
        ));
        // Nothing half-made was left behind by the refusals.
        assert!(store.moves().await.unwrap().is_empty());
    }
}
