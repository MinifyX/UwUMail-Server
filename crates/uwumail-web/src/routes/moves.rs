//! Server → Accounts & domains → Moves (docs/moving.md, "For admins"): the admin moves a whole
//! domain with everyone on it, or one mailbox, from another server.
//!
//! Starting a move creates what is missing here first -- the domain (with its DKIM keys), the
//! mailboxes (without a password: their people get a link to choose one), the aliases -- and then
//! hands the old logins to the worker in the server crate. Every row is checked before anything
//! is made, so a list with mistakes changes nothing and says which lines to fix.
//!
//! Passwords of the old mailboxes go in and never come out: nothing here returns them, and
//! finishing a move wipes them.

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uwumail_store::{
    DEFAULT_MOVE_PARALLEL, DEFAULT_MOVE_SYNC_MINUTES, DavKind, DavMode, MAX_MOVE_MAILBOXES, MAX_MOVE_PARALLEL,
    MAX_MOVE_SYNC_MINUTES, MIN_MOVE_SYNC_MINUTES, Move, MoveKind, MoveSettings, NewAccount, NewDavCollection,
    NewImportCollection, NewMove, NewMoveMailbox, PasswordLinkPurpose, Person, Role, decode_text, split_ics, split_vcf,
};

use super::audit;
use super::moves_csv::{self, CsvRead};
use super::people::PASSWORD_LINK_LIFETIME_SECS;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

/// The largest file of contacts or calendars taken for one mailbox.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
/// A list of people as JSON or CSV: two thousand rows with room to spare.
pub const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
/// Looking for the old server asks other servers; a few dozen an hour are plenty.
const LOOKUPS_PER_HOUR: usize = 30;
/// The IMAP port with TLS from the first byte; the only way the mover connects.
const DEFAULT_PORT: u16 = 993;

async fn load(web: &Web, id: i64) -> ApiResult<Move> {
    web.store().move_by_id(id).await?.ok_or_else(|| ApiError::NotFound(format!("move {id}")))
}

fn limits() -> Value {
    json!({
        "maxMailboxes": MAX_MOVE_MAILBOXES,
        "maxParallel": MAX_MOVE_PARALLEL,
        "defaultParallel": DEFAULT_MOVE_PARALLEL,
        "minSyncMinutes": MIN_MOVE_SYNC_MINUTES,
        "maxSyncMinutes": MAX_MOVE_SYNC_MINUTES,
        "defaultSyncMinutes": DEFAULT_MOVE_SYNC_MINUTES,
        "maxUploadBytes": MAX_UPLOAD_BYTES,
    })
}

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let moves = web.store().moves().await?;
    Ok(Json(json!({ "moves": moves, "limits": limits() })))
}

/// A move with its mailboxes; each says whether its person chose a password already (then no
/// link is needed for them).
async fn detail_json(web: &Web, id: i64) -> ApiResult<Value> {
    let found = load(web, id).await?;
    let mailboxes = web.store().move_mailboxes(id).await?;
    let people: HashMap<String, Person> =
        web.store().people().await?.into_iter().map(|person| (person.account.login.clone(), person)).collect();
    let mailboxes: Vec<Value> = mailboxes
        .iter()
        .map(|mailbox| {
            let mut value = json!(mailbox);
            let person = people.get(&mailbox.address);
            value["hasPortalPassword"] = json!(person.is_none_or(|person| person.has_password));
            value["aliases"] = json!(
                person
                    .map(|person| person
                        .addresses
                        .iter()
                        .filter(|address| address.kind == "alias")
                        .map(|address| address.address.clone())
                        .collect::<Vec<_>>())
                    .unwrap_or_default()
            );
            value
        })
        .collect();
    Ok(json!({ "move": found, "mailboxes": mailboxes, "limits": limits(), "hostname": web.settings().hostname }))
}

pub async fn detail(State(web): State<Web>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(detail_json(&web, id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lookup {
    /// The domain, or an address on it.
    address: String,
}

/// Suggests where the old server is, from the domain alone: its SRV records, the provider's
/// autoconfig file, the usual names. Nobody logs in; the first round of the move does.
pub async fn discover(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(lookup): Json<Lookup>,
) -> ApiResult<Json<Value>> {
    let input = lookup.address.trim().to_lowercase();
    let address = if input.contains('@') { input.clone() } else { format!("postmaster@{input}") };
    let domain = address.rsplit_once('@').map(|(_, domain)| domain.to_owned()).unwrap_or_default();
    if uwumail_store::normalize_address(&address).is_err() {
        return Err(ApiError::Rule("senderInvalid", format!("'{input}' is not a domain")));
    }
    if !web.allow_call(session.account.id, "moveLookup", LOOKUPS_PER_HOUR) {
        return Err(ApiError::TooManyAttempts);
    }
    let found = uwumail_smtp::autoconfig::suggest(web.smtp(), web.dns(), &address).await;
    let imap = found
        .first()
        .map(|settings| json!({ "host": settings.imap.host, "port": settings.imap.port, "source": settings.source }));
    let dav = match uwumail_dav::client::provider_of(&domain).map(|provider| provider.name) {
        Some("iCloud") => "icloud",
        Some("GMX") => "gmx",
        Some("WEB.DE") => "webde",
        Some("Google" | "Outlook.com") => "none",
        _ => "auto",
    };
    Ok(Json(json!({ "imap": imap, "davMode": dav, "domainHere": web.store().is_local_domain(&domain).await? })))
}

#[derive(Deserialize)]
pub struct CsvBody {
    text: String,
    domain: Option<String>,
}

/// Reads a pasted or uploaded CSV list into rows for the table, with the lines that need fixing.
pub async fn read_csv(_admin: Admin, Json(body): Json<CsvBody>) -> ApiResult<Json<CsvRead>> {
    let domain = body.domain.as_deref().map(str::trim).filter(|domain| !domain.is_empty());
    Ok(Json(moves_csv::read(&body.text, domain, MAX_MOVE_MAILBOXES)))
}

/// One person of a move, as the table or the CSV list has them.
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Row {
    old_address: String,
    /// Empty: the old address.
    login: String,
    password: String,
    name: String,
    /// The address here; the old address's local part on the domain when empty.
    target: String,
    quota_bytes: Option<i64>,
    aliases: Vec<String>,
    /// Its own old server, when it is not the move's.
    imap_host: String,
    imap_port: Option<u16>,
    dav_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowProblem {
    /// The row's place in the list, from 0.
    row: usize,
    field: &'static str,
    code: &'static str,
}

/// What a row will do, for the check before starting.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Planned {
    row: usize,
    target: String,
    /// The mailbox is there already and is filled; otherwise it is made.
    exists: bool,
    /// Its person has a password here already, so no link is needed.
    has_password: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMoveBody {
    kind: String,
    domain: String,
    #[serde(default)]
    imap_host: String,
    imap_port: Option<u16>,
    #[serde(default)]
    dav_mode: String,
    #[serde(default)]
    dav_host: String,
    #[serde(default)]
    dav_url: String,
    #[serde(default = "yes")]
    contacts: bool,
    #[serde(default = "yes")]
    calendars: bool,
    parallel: Option<i64>,
    sync_minutes: Option<i64>,
    rows: Vec<Row>,
    /// Only check, and say what would happen.
    #[serde(default)]
    dry_run: bool,
}

fn yes() -> bool {
    true
}

fn normalized(address: &str) -> Option<String> {
    uwumail_store::normalize_address(address.trim()).ok().map(|(local, domain)| format!("{local}@{domain}"))
}

/// Who has which address here: the login of the account holding it.
fn address_owners(people: &[Person]) -> HashMap<String, String> {
    people
        .iter()
        .flat_map(|person| {
            person.addresses.iter().map(move |address| (address.address.clone(), person.account.login.clone()))
        })
        .collect()
}

/// Checks every row against the domain and what is here already. Rows come back normalized.
async fn check_rows(
    web: &Web,
    domain: &str,
    rows: &mut [Row],
    people: &[Person],
) -> ApiResult<(Vec<RowProblem>, Vec<Planned>)> {
    let owners = address_owners(people);
    let by_login: HashMap<&str, &Person> = people.iter().map(|p| (p.account.login.as_str(), p)).collect();
    let busy = web.store().accounts_in_open_moves().await?;
    let local_domains: HashSet<String> = web.store().domains().await?.into_iter().map(|d| d.name).collect();
    let mut problems = Vec::new();
    let mut planned = Vec::new();
    let mut seen_targets = HashSet::new();
    let mut seen_aliases = HashSet::new();
    for (index, row) in rows.iter_mut().enumerate() {
        let mut problem = |field, code| problems.push(RowProblem { row: index, field, code });
        match normalized(&row.old_address) {
            Some(address) => row.old_address = address,
            None => problem("oldAddress", "addressInvalid"),
        }
        row.login = row.login.trim().to_owned();
        if row.login.chars().count() > 320 || row.login.chars().any(char::is_control) {
            problem("login", "loginInvalid");
        }
        if row.password.is_empty() || row.password.len() > 1024 || row.password.contains(['\r', '\n', '\0']) {
            problem("password", "passwordMissing");
        }
        if row.target.trim().is_empty()
            && let Some((local, _)) = row.old_address.rsplit_once('@')
        {
            row.target = format!("{local}@{domain}");
        }
        match normalized(&row.target) {
            Some(target) if target.ends_with(&format!("@{domain}")) => row.target = target,
            _ => problem("target", "targetInvalid"),
        }
        if !seen_targets.insert(row.target.clone()) {
            problem("target", "duplicate");
        }
        let person = by_login.get(row.target.as_str()).copied();
        match (person, owners.get(&row.target)) {
            (Some(person), _) => {
                if person.account.deleted_at.is_some() || !person.account.has_mailbox() {
                    problem("target", "noMailbox");
                }
                if busy.contains(&person.account.id) {
                    problem("target", "mailboxBusy");
                }
            }
            // An alias of somebody else.
            (None, Some(_)) => problem("target", "targetTaken"),
            (None, None) => {}
        }
        if row.quota_bytes.is_some_and(|quota| quota < 0) {
            problem("quota", "quotaInvalid");
        }
        let mut aliases = Vec::new();
        for alias in &row.aliases {
            let Some(alias) = normalized(alias) else {
                problem("aliases", "aliasInvalid");
                continue;
            };
            let alias_domain = alias.rsplit_once('@').map(|(_, d)| d.to_owned()).unwrap_or_default();
            if alias_domain != domain && !local_domains.contains(&alias_domain) {
                problem("aliases", "aliasDomain");
            }
            match owners.get(&alias) {
                Some(owner) if *owner != row.target => problem("aliases", "aliasTaken"),
                _ if alias == row.target || !seen_aliases.insert(alias.clone()) => problem("aliases", "duplicate"),
                _ => {}
            }
            aliases.push(alias);
        }
        if aliases.len() > 20 {
            problem("aliases", "aliasInvalid");
        }
        row.aliases = aliases;
        if !row.imap_host.trim().is_empty() {
            match uwumail_store::check_server_name(&row.imap_host) {
                Ok(host) => row.imap_host = host,
                Err(_) => problem("imapHost", "hostInvalid"),
            }
        }
        if row.imap_port == Some(0) {
            problem("imapHost", "hostInvalid");
        }
        match uwumail_store::check_server_url(&row.dav_url) {
            Ok(url) => row.dav_url = url,
            Err(_) => problem("davUrl", "urlInvalid"),
        }
        planned.push(Planned {
            row: index,
            target: row.target.clone(),
            exists: person.is_some(),
            has_password: person.is_some_and(|person| person.has_password),
        });
    }
    Ok((problems, planned))
}

fn blocked(problems: Vec<RowProblem>) -> ApiError {
    ApiError::Blocked("moveRows", format!("{} problems in the list", problems.len()), json!(problems))
}

/// Makes the mailboxes and aliases the rows need, and says which mailbox each row goes into.
async fn make_mailboxes(
    web: &Web,
    session: &crate::session::Session,
    rows: &[Row],
    people: &[Person],
) -> ApiResult<Vec<NewMoveMailbox>> {
    let owners = address_owners(people);
    let mut mailboxes = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let (account_id, created) = match web.store().account(&row.target).await? {
            Some(account) => (account.id, false),
            None => {
                let new = NewAccount {
                    address: row.target.clone(),
                    display_name: row.name.trim().to_owned(),
                    password: None,
                    role: Role::User,
                    quota_bytes: row.quota_bytes.unwrap_or(0),
                    protocols: None,
                };
                let account = web.store().create_account(new).await.map_err(|err| {
                    tracing::warn!(%err, target = %row.target, "a mailbox for a move could not be made");
                    blocked(vec![RowProblem { row: index, field: "target", code: "targetTaken" }])
                })?;
                let details =
                    json!({ "role": account.role, "quotaBytes": account.quota_bytes, "invited": true, "move": true });
                audit(web, session, "account.create", &account.login, details).await;
                (account.id, true)
            }
        };
        for alias in &row.aliases {
            if owners.get(alias).is_some_and(|owner| *owner == row.target) {
                continue;
            }
            web.store().add_alias(alias, &row.target).await.map_err(|err| {
                tracing::warn!(%err, alias, "an alias for a move could not be added");
                blocked(vec![RowProblem { row: index, field: "aliases", code: "aliasTaken" }])
            })?;
            audit(web, session, "alias.add", alias, json!({ "account": row.target, "move": true })).await;
        }
        mailboxes.push(NewMoveMailbox {
            account_id,
            old_address: row.old_address.clone(),
            login: row.login.clone(),
            password: row.password.clone(),
            imap_host: Some(row.imap_host.clone()).filter(|host| !host.is_empty()),
            imap_port: row.imap_port,
            dav_url: row.dav_url.clone(),
            created_account: created,
        });
    }
    Ok(mailboxes)
}

/// Starts a move, or with `dryRun` only checks it and says what would happen.
pub async fn create(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(mut body): Json<NewMoveBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let kind = MoveKind::parse(&body.kind).ok_or_else(|| ApiError::Invalid("kind is domain or mailbox".into()))?;
    let domain = uwumail_store::normalize_domain(&body.domain)?;
    let dav_mode = DavMode::parse(body.dav_mode.trim()).ok_or_else(|| ApiError::Invalid("unknown dav mode".into()))?;
    if body.rows.is_empty() {
        return Err(ApiError::Invalid("a move needs at least one mailbox".into()));
    }
    if body.rows.len() > MAX_MOVE_MAILBOXES {
        return Err(ApiError::Rule("moveTooMany", format!("at most {MAX_MOVE_MAILBOXES} mailboxes in one move")));
    }
    if kind == MoveKind::Mailbox && body.rows.len() != 1 {
        return Err(ApiError::Invalid("a single move takes exactly one mailbox".into()));
    }
    let imap_host = uwumail_store::check_server_name(&body.imap_host)
        .map_err(|_| ApiError::Rule("hostInvalid", "the old server's name is not usable".into()))?;
    let parallel = body.parallel.unwrap_or(DEFAULT_MOVE_PARALLEL);
    let sync_minutes = body.sync_minutes.unwrap_or(DEFAULT_MOVE_SYNC_MINUTES);
    let existing = web.store().domain(&domain).await?;
    if existing.as_ref().is_some_and(|d| d.kind != uwumail_store::DomainKind::Mail) {
        return Err(ApiError::Rule("domainMaskedOnly", format!("{domain} takes masked addresses only")));
    }
    let people = web.store().people().await?;
    let (problems, planned) = check_rows(&web, &domain, &mut body.rows, &people).await?;
    if body.dry_run {
        return Ok((
            StatusCode::OK,
            Json(json!({ "domainExists": existing.is_some(), "rows": planned, "problems": problems })),
        ));
    }
    if !problems.is_empty() {
        return Err(blocked(problems));
    }
    // The move's own settings are checked before anything is made.
    if !(1..=MAX_MOVE_PARALLEL).contains(&parallel)
        || !(MIN_MOVE_SYNC_MINUTES..=MAX_MOVE_SYNC_MINUTES).contains(&sync_minutes)
    {
        return Err(ApiError::Invalid("the speed or the rhythm of the move is out of range".into()));
    }
    let dav_url = uwumail_store::check_server_url(&body.dav_url)
        .map_err(|_| ApiError::Rule("urlInvalid", "the CalDAV/CardDAV address is not usable".into()))?;
    if dav_mode == DavMode::Custom && dav_url.is_empty() {
        return Err(ApiError::Rule("urlInvalid", "the CalDAV/CardDAV address is missing".into()));
    }
    let dav_host = match body.dav_host.trim() {
        "" => String::new(),
        host => uwumail_store::check_server_name(host)
            .map_err(|_| ApiError::Rule("hostInvalid", "the CalDAV/CardDAV server's name is not usable".into()))?,
    };
    if existing.is_none() {
        let made = web.store().create_domain(&domain).await?;
        if let Err(err) = uwumail_smtp::dkim::ensure_domain_keys(web.store(), &made.name).await {
            tracing::error!(%err, domain = %made.name, "creating DKIM keys failed");
            return Err(ApiError::Internal);
        }
        audit(&web, &session, "domain.create", &made.name, json!({ "kind": made.kind, "move": true })).await;
    }
    let mailboxes = make_mailboxes(&web, &session, &body.rows, &people).await?;
    let new = NewMove {
        kind,
        domain: domain.clone(),
        imap_host,
        imap_port: body.imap_port.unwrap_or(DEFAULT_PORT),
        dav_mode,
        dav_host,
        dav_url,
        contacts: body.contacts,
        calendars: body.calendars,
        parallel,
        sync_minutes,
        created_by: Some(session.account.id),
    };
    let created = web.store().create_move(new, mailboxes).await?;
    let details = json!({
        "id": created.id,
        "kind": created.kind,
        "from": created.imap_host,
        "mailboxes": created.summary.mailboxes,
        "domainCreated": existing.is_none(),
    });
    audit(&web, &session, "move.create", &domain, details).await;
    Ok((StatusCode::CREATED, Json(detail_json(&web, created.id).await?)))
}

#[derive(Deserialize)]
pub struct MoreRows {
    rows: Vec<Row>,
}

/// More people for a domain move that is still copying.
pub async fn add_mailboxes(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
    Json(mut body): Json<MoreRows>,
) -> ApiResult<Json<Value>> {
    let found = load(&web, id).await?;
    if body.rows.is_empty() {
        return Err(ApiError::Invalid("no mailboxes to add".into()));
    }
    if found.summary.mailboxes as usize + body.rows.len() > MAX_MOVE_MAILBOXES {
        return Err(ApiError::Rule("moveTooMany", format!("at most {MAX_MOVE_MAILBOXES} mailboxes in one move")));
    }
    if found.kind == MoveKind::Mailbox {
        return Err(ApiError::Rule("moveSingle", "a single move takes exactly one mailbox".into()));
    }
    if matches!(found.state, uwumail_store::MoveState::Finishing | uwumail_store::MoveState::Done) {
        return Err(ApiError::Rule("moveFinished", "this move is finished".into()));
    }
    let people = web.store().people().await?;
    let (problems, _) = check_rows(&web, &found.domain, &mut body.rows, &people).await?;
    if !problems.is_empty() {
        return Err(blocked(problems));
    }
    let mailboxes = make_mailboxes(&web, &session, &body.rows, &people).await?;
    let count = mailboxes.len();
    web.store().add_move_mailboxes(id, mailboxes).await?;
    audit(&web, &session, "move.addMailboxes", &found.domain, json!({ "id": id, "mailboxes": count })).await;
    Ok(Json(detail_json(&web, id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBody {
    parallel: Option<i64>,
    sync_minutes: Option<i64>,
}

pub async fn change(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Json<Value>> {
    let changed = web
        .store()
        .change_move_settings(id, MoveSettings { parallel: body.parallel, sync_minutes: body.sync_minutes })
        .await?;
    let details = json!({ "id": id, "parallel": changed.parallel, "syncMinutes": changed.sync_minutes });
    audit(&web, &session, "move.settings", &changed.domain, details).await;
    Ok(Json(detail_json(&web, id).await?))
}

pub async fn pause(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let paused = web.store().pause_move(id).await?;
    audit(&web, &session, "move.pause", &paused.domain, json!({ "id": id })).await;
    Ok(Json(detail_json(&web, id).await?))
}

pub async fn resume(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let resumed = web.store().resume_move(id).await?;
    audit(&web, &session, "move.resume", &resumed.domain, json!({ "id": id })).await;
    Ok(Json(detail_json(&web, id).await?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FinishBody {
    /// Wipe the passwords at once instead of a last round.
    skip_last_round: bool,
}

/// Finishes a move after the MX records point here: a last round for every mailbox, then the
/// passwords are wiped.
pub async fn finish(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
    Json(body): Json<FinishBody>,
) -> ApiResult<Json<Value>> {
    let finished = web.store().finish_move(id, body.skip_last_round).await?;
    let details = json!({ "id": id, "skipLastRound": body.skip_last_round });
    audit(&web, &session, "move.finish", &finished.domain, details).await;
    Ok(Json(detail_json(&web, id).await?))
}

/// Forgets a move with every password in it; the mailboxes and their mail stay.
pub async fn remove(State(web): State<Web>, Admin(session): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let found = load(&web, id).await?;
    web.store().delete_move(id).await?;
    audit(&web, &session, "move.delete", &found.domain, json!({ "id": id, "state": found.state })).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct RetryBody {
    login: Option<String>,
    password: Option<String>,
}

/// One mailbox again: after a pause it goes on, when it is up to date a round starts now.
pub async fn retry_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((id, mailbox)): Path<(i64, i64)>,
    Json(body): Json<RetryBody>,
) -> ApiResult<Json<Value>> {
    let new_password = body.password.as_deref().is_some_and(|password| !password.is_empty());
    let retried = web.store().retry_move_mailbox(id, mailbox, body.login, body.password).await?;
    let details = json!({ "id": id, "newPassword": new_password });
    audit(&web, &session, "move.retry", &retried.address, details).await;
    Ok(Json(detail_json(&web, id).await?))
}

pub async fn pause_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((id, mailbox)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    let paused = web.store().pause_move_mailbox(id, mailbox).await?;
    audit(&web, &session, "move.pauseMailbox", &paused.address, json!({ "id": id })).await;
    Ok(Json(detail_json(&web, id).await?))
}

pub async fn remove_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((id, mailbox)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    let found = web
        .store()
        .move_mailbox(Some(id), mailbox)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("mailbox {mailbox} of the move")))?;
    web.store().remove_move_mailbox(id, mailbox).await?;
    audit(&web, &session, "move.removeMailbox", &found.address, json!({ "id": id })).await;
    Ok(Json(detail_json(&web, id).await?))
}

#[derive(Deserialize)]
pub struct UploadTarget {
    kind: String,
}

/// Contacts or calendars of one mailbox as a file, for an old provider that does not hand them out
/// over CardDAV/CalDAV.
pub async fn upload(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((id, mailbox)): Path<(i64, i64)>,
    Query(query): Query<UploadTarget>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let found = web
        .store()
        .move_mailbox(Some(id), mailbox)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("mailbox {mailbox} of the move")))?;
    let kind = match query.kind.as_str() {
        "calendar" => DavKind::Calendar,
        "addressbook" => DavKind::Addressbook,
        _ => return Err(ApiError::Invalid("kind is calendar or addressbook".into())),
    };
    if body.is_empty() {
        return Err(ApiError::Rule("importEmpty", "the file is empty".into()));
    }
    let text = decode_text(&body);
    let (calendar, book) = web.smtp().tone().language.collection_names();
    let (split, default) = match kind {
        DavKind::Calendar => (split_ics(&text, false), NewDavCollection::default_calendar(calendar)),
        DavKind::Addressbook => (split_vcf(&text), NewDavCollection::default_address_book(book)),
    };
    if !split.recognized {
        return Err(match kind {
            DavKind::Calendar => ApiError::Rule("notICalendar", "this is not an iCalendar file".into()),
            DavKind::Addressbook => ApiError::Rule("notVCard", "this is not a vCard file".into()),
        });
    }
    let new = NewImportCollection {
        name: split.meta.name.clone().unwrap_or_else(|| default.display_name.clone()),
        description: split.meta.description.clone().unwrap_or_default(),
        color: split.meta.color.clone(),
    };
    let (collection, report) = web.store().move_dav_import(found.account_id, kind, new, default, split).await?;
    let count = (report.created + report.updated + report.unchanged) as i64;
    let (contacts, events) = if kind == DavKind::Addressbook { (count, 0) } else { (0, count) };
    web.store().count_move_objects(mailbox, contacts, events).await?;
    let details = json!({ "id": id, "kind": kind, "entries": count });
    audit(&web, &session, "move.upload", &found.address, details).await;
    Ok(Json(json!({ "report": report, "collection": collection.display_name })))
}

/// The MX check of the move's domain: whether mail comes here already. The move is finished once
/// it does.
pub async fn mx(State(web): State<Web>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let found = load(&web, id).await?;
    let Some(dns) = web.dns() else {
        return Err(ApiError::Rule("dnsUnavailable", "DNS lookups are not available on this server".into()));
    };
    let hostname = web.settings().hostname.clone();
    let check = dns.check_mx(&found.domain, &hostname, web.smtp().behind_upstream_server()).await;
    Ok(Json(json!({ "check": check, "hostname": hostname })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LinksBody {
    /// Only these mailboxes of the move; all when empty.
    mailboxes: Vec<i64>,
}

/// Links to choose a password, for every mailbox of the move whose person has none yet. Making
/// them again replaces the ones before (each works seven days). Mailboxes whose person has a
/// password -- the ones that were filled -- get none, and say so.
pub async fn links(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
    Json(body): Json<LinksBody>,
) -> ApiResult<Json<Value>> {
    load(&web, id).await?;
    let wanted: HashSet<i64> = body.mailboxes.into_iter().collect();
    let people: HashMap<String, Person> =
        web.store().people().await?.into_iter().map(|person| (person.account.login.clone(), person)).collect();
    let mut links = Vec::new();
    let mut skipped = Vec::new();
    for mailbox in web.store().move_mailboxes(id).await? {
        if !wanted.is_empty() && !wanted.contains(&mailbox.id) {
            continue;
        }
        let Some(person) = people.get(&mailbox.address) else { continue };
        let reason = if person.has_password {
            Some("hasPassword")
        } else if !person.account.can_use_portal() {
            Some("disabled")
        } else if web.store().auth_source(person.account.id).await? == "ldap" {
            Some("directory")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push(json!({ "mailboxId": mailbox.id, "address": mailbox.address, "reason": reason }));
            continue;
        }
        let (token, expires_at) = web
            .store()
            .create_password_link(
                &mailbox.address,
                PasswordLinkPurpose::Invite,
                Some(session.account.id),
                PASSWORD_LINK_LIFETIME_SECS,
            )
            .await?;
        audit(&web, &session, "account.passwordLink", &mailbox.address, json!({ "purpose": "invite", "move": id }))
            .await;
        links.push(json!({
            "mailboxId": mailbox.id,
            "address": mailbox.address,
            "name": person.account.display_name,
            "oldAddress": mailbox.old_address,
            "path": format!("/password/{token}"),
            "expiresAt": expires_at,
        }));
    }
    Ok(Json(json!({ "links": links, "skipped": skipped, "hostname": web.settings().hostname })))
}
