//! The moves the admin runs (docs/moving.md, "For admins"): every mailbox of a domain, or one,
//! copied from another server -- mail over IMAP with the same code as a person's own move
//! (`crate::migrate`), contacts and calendars over CardDAV/CalDAV with the same login.
//!
//! A few mailboxes at a time: each move says how many of its mailboxes may be copied at once, and
//! the whole server copies at most [`MAX_TURNS`] at once, so neither the old server nor this one is
//! overrun. A turn has a time slice like a person's move; a mailbox that is not through goes back
//! in the queue and the next one gets its turn. When everything is here the mailbox is synced and
//! waits for its next round, which brings only what arrived at the old server since -- until the
//! admin finishes the move. Then one last round runs, contacts and calendars come once more, and
//! the password is wiped.
//!
//! Contacts and calendars come in the first complete round and the last one. Where they were
//! found is kept, so the last round asks the same addresses even when the domain's DNS points here
//! by then. When they cannot be found, the mail still moves; the admin can upload files instead.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinSet;
use uwumail_dav::client::{self, Remote, RemoteCollection, RemoteError, Transport};
use uwumail_smtp::dnscheck::DnsChecker;
use uwumail_store::{
    DavKind, DavMode, MigrationProgress, MigrationRun, Move, MoveMailbox, MoveTurn, NewDavCollection,
    NewImportCollection, SkippedOf, Store, split_ics, split_vcf,
};

use crate::fetch::Detour;
use crate::import::imap::{Connection, CopyOptions, Token};
use crate::migrate::{Objects, OldMailbox, connect, copy_with, paused};

/// Turns that run at once on the whole server.
const MAX_TURNS: usize = 4;
/// How often the queue is looked at.
const TICK: Duration = Duration::from_secs(5);
/// How long one mailbox is worked on before the next one gets its turn.
const RUN_LIMIT: Duration = Duration::from_secs(300);
/// On top of the time slice: the portion that was being fetched may still finish.
const GRACE: Duration = Duration::from_secs(300);
/// Bringing the contacts and calendars of one mailbox over, at most.
const DAV_LIMIT: Duration = Duration::from_secs(300);

/// What a turn needs besides the store.
#[derive(Clone)]
pub(crate) struct Env {
    pub transport: Arc<dyn Transport>,
    pub dns: Option<Arc<DnsChecker>>,
    pub dialer: Option<uwumail_smtp::egress::Dialer>,
    /// For tests: the old IMAP server at a local address with its own certificate.
    pub detour: Option<Detour>,
    /// What the first calendar and address book of an account are called.
    pub names: (String, String),
    pub limit: Duration,
    pub grace: Duration,
    /// The largest message this server takes; larger ones are left out (0: no limit of its own).
    pub max_size: usize,
}

/// Works through the mailboxes of the admin's moves until `shutdown` changes.
pub async fn run_moves(
    store: Store,
    smtp: uwumail_smtp::Smtp,
    egress: uwumail_smtp::egress::Egress,
    mut shutdown: watch::Receiver<bool>,
) {
    // A clean stop requeues what was running, so what is still running now was cut off by a crash.
    match store.recover_interrupted_moves().await {
        Ok((0, 0)) => {}
        Ok((count, paused)) => {
            tracing::info!(count, "moves of the admin continue after the restart");
            if paused > 0 {
                tracing::warn!(paused, "mailboxes the server went down during again and again are paused");
            }
        }
        Err(err) => tracing::warn!(%err, "looking for interrupted moves failed"),
    }
    let dialer = egress.dialer(uwumail_smtp::egress::Purpose::Fetch);
    let (calendar, book) = smtp.tone().language.collection_names();
    let mut env = Env {
        transport: Arc::new(client::HttpsTransport::new(&dialer)),
        dns: DnsChecker::new().ok().map(Arc::new),
        dialer: Some(dialer),
        detour: None,
        names: (calendar.to_owned(), book.to_owned()),
        limit: RUN_LIMIT,
        grace: GRACE,
        max_size: 0,
    };
    let mut turns = JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        env.max_size = smtp.max_message_size();
        if let Err(err) = store.queue_due_move_mailboxes().await {
            tracing::warn!(%err, "queueing the next rounds of moves failed");
        }
        while turns.len() < MAX_TURNS {
            let mailbox = match store.take_move_mailbox().await {
                Ok(Some(mailbox)) => mailbox,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(%err, "reading the queued moves failed");
                    break;
                }
            };
            let (store, env) = (store.clone(), env.clone());
            turns.spawn(async move { turn(&store, &env, mailbox).await });
        }
        tokio::select! {
            Some(_) = turns.join_next(), if !turns.is_empty() => {}
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => {}
        }
    }
    turns.abort_all();
    while turns.join_next().await.is_some() {}
    // Whatever was cut off goes on where its folders stood at the next start.
    let _ = store.requeue_running_moves().await;
}

/// One turn on one mailbox, handed back to the store afterwards.
pub(crate) async fn turn(store: &Store, env: &Env, mailbox: MoveMailbox) {
    let last_round = mailbox.final_round;
    let result = run_turn(store, env, &mailbox).await;
    if let Err(err) = store.finish_move_turn(mailbox.id, result, last_round).await {
        tracing::warn!(mailbox = %mailbox.address, %err, "writing down how a move went failed");
    }
}

fn turn_of(run: MigrationRun) -> MoveTurn {
    match run {
        MigrationRun::Continue => MoveTurn::Continue,
        MigrationRun::Done => MoveTurn::RoundDone,
        MigrationRun::Paused { code, detail } => MoveTurn::Paused { code, detail },
    }
}

/// Copies one mailbox for up to the time slice and says how it ended. Never fails: whatever went
/// wrong becomes a pause with a reason.
pub(crate) async fn run_turn(store: &Store, env: &Env, mailbox: &MoveMailbox) -> MoveTurn {
    let found = match store.move_by_id(mailbox.move_id).await {
        Ok(Some(found)) => found,
        Ok(None) => return MoveTurn::Continue,
        Err(err) => return turn_of(paused("failed", err)),
    };
    let password = match store.move_mailbox_password(mailbox.id).await {
        Ok(Some(password)) => password,
        // Wiped meanwhile: the move was finished without a last round (then the mailbox is done
        // and the pause changes nothing), or its account went to the trash. Never queued again
        // without a password, which would only spin.
        Ok(None) => return turn_of(paused("accountDeleted", "the password of the old mailbox was wiped")),
        Err(err) => return turn_of(paused("failed", err)),
    };
    let host = if mailbox.imap_host.is_empty() { found.imap_host.clone() } else { mailbox.imap_host.clone() };
    let port = if mailbox.imap_port == 0 { found.imap_port } else { mailbox.imap_port };
    let old = OldMailbox { account_id: mailbox.account_id, host: &host, port, login: &mailbox.login };
    let deadline = tokio::time::Instant::now() + env.limit + env.grace;
    let connecting = connect(store, &old, password.clone(), env.detour.clone(), env.dialer.clone());
    let mut connection = match tokio::time::timeout_at(deadline, connecting).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(run)) => return turn_of(run),
        Err(_) => return turn_of(paused("unreachable", format!("{host} did not answer in time"))),
    };
    if mailbox.source_bytes.is_none()
        && let Ok(Some(bytes)) = tokio::time::timeout_at(deadline, source_size(&mut connection)).await
    {
        let _ = store.note_move_source_size(mailbox.id, bytes).await;
    }
    let id = mailbox.id;
    let note =
        Box::new(move |store: Store, progress: MigrationProgress| -> Pin<Box<dyn Future<Output = bool> + Send>> {
            Box::pin(async move { store.note_move_progress(id, progress).await.unwrap_or(true) })
        });
    let wants_objects = found.contacts || found.calendars;
    let objects: Option<Box<Objects>> = wants_objects.then(|| {
        let store = store.clone();
        let names = env.names.clone();
        let (account_id, contacts, calendars) = (mailbox.account_id, found.contacts, found.calendars);
        Box::new(move |kind: DavKind, folder: String, texts: Vec<String>| -> Pin<Box<dyn Future<Output = bool> + Send>> {
            let store = store.clone();
            let names = names.clone();
            Box::pin(async move {
                let wanted = match kind {
                    DavKind::Addressbook => contacts,
                    DavKind::Calendar => calendars,
                };
                // Not asked for (the copy only offers kinds asked for): the messages stay mail.
                if !wanted {
                    return false;
                }
                let name = uwumail_imap::mutf7::decode(&folder).unwrap_or(folder);
                let name = name.rsplit(['/', '.']).next().unwrap_or_default().to_owned();
                let new = NewImportCollection { name, description: String::new(), color: None };
                match import(&store, account_id, kind, new, &names, texts.concat()).await {
                    Ok(count) => {
                        let (contacts, events) = if kind == DavKind::Addressbook { (count, 0) } else { (0, count) };
                        let _ = store.count_move_objects(id, contacts, events).await;
                        true
                    }
                    // Copied as mail instead, so nothing is lost (security review 0.22 MOV-1).
                    Err(err) => {
                        tracing::warn!(%err, "contacts or calendars from an IMAP folder could not be stored, kept as mail");
                        false
                    }
                }
            })
        }) as Box<Objects>
    });
    let options = CopyOptions {
        skip_known: true,
        contacts: found.contacts,
        calendars: found.calendars,
        max_size: env.max_size,
        ..CopyOptions::default()
    };
    let source_name = mailbox.source_name(&host);
    let copy = copy_with(
        store,
        &mut connection,
        mailbox.account_id,
        &source_name,
        mailbox.progress,
        options,
        env.limit,
        note,
        objects,
        SkippedOf::MoveMailbox(mailbox.id),
    );
    let run = tokio::time::timeout_at(deadline, copy).await.unwrap_or(MigrationRun::Continue);
    logout(&mut connection).await;
    // Contacts and calendars in the first complete round and the last one.
    if run == MigrationRun::Done && (mailbox.rounds == 0 || mailbox.final_round) && wants_objects {
        let pulled = tokio::time::timeout(DAV_LIMIT, pull_dav(store, env, &found, mailbox, &password)).await;
        let (contacts, events, error, kept) = match pulled {
            Ok(pulled) => pulled,
            Err(_) => (0, 0, RemoteError::Timeout.code().to_owned(), None),
        };
        if let Err(err) = store.note_move_dav(mailbox.id, contacts, events, &error, kept).await {
            tracing::warn!(%err, "writing down how contacts and calendars went failed");
        }
    }
    turn_of(run)
}

async fn logout(connection: &mut Connection) {
    let _ = tokio::time::timeout(Duration::from_secs(10), connection.command("LOGOUT")).await;
}

/// What the old mailbox holds altogether, from its quota (RFC 9208), when the server says.
async fn source_size(connection: &mut Connection) -> Option<i64> {
    let responses = connection.command("GETQUOTAROOT INBOX").await.ok()?;
    responses.iter().find_map(|response| {
        let tokens = &response.tokens;
        if tokens.get(1) != Some(&Token::Atom("QUOTA".into())) {
            return None;
        }
        let position = tokens
            .iter()
            .position(|token| matches!(token, Token::Atom(word) if word.eq_ignore_ascii_case("STORAGE")))?;
        let used: i64 = tokens.get(position + 1)?.text()?.parse().ok()?;
        Some(used.saturating_mul(1024))
    })
}

/// Stores the entries of one calendar or address book text; how many came in.
async fn import(
    store: &Store,
    account_id: i64,
    kind: DavKind,
    new: NewImportCollection,
    names: &(String, String),
    text: String,
) -> uwumail_store::Result<i64> {
    let (split, default) = match kind {
        DavKind::Calendar => (split_ics(&text, false), NewDavCollection::default_calendar(&names.0)),
        DavKind::Addressbook => (split_vcf(&text), NewDavCollection::default_address_book(&names.1)),
    };
    let (_, report) = store.move_dav_import(account_id, kind, new, default, split).await?;
    Ok((report.created + report.updated + report.unchanged) as i64)
}

/// A collection found at the old provider, as kept between rounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    url: String,
    kind: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color: Option<String>,
}

impl Kept {
    fn of(found: &RemoteCollection) -> Kept {
        Kept {
            url: found.url.to_string(),
            kind: found.kind.as_str().to_owned(),
            name: found.name.clone(),
            description: found.description.clone(),
            color: found.color.clone(),
        }
    }

    fn collection(&self) -> Option<RemoteCollection> {
        let kind = match self.kind.as_str() {
            "calendar" => DavKind::Calendar,
            "addressbook" => DavKind::Addressbook,
            _ => return None,
        };
        // Checked again like any address from outside: https, public, no login inside.
        let url = client::checked_url(&self.url).ok()?;
        Some(RemoteCollection {
            url,
            kind,
            name: self.name.clone(),
            description: self.description.clone(),
            color: self.color.clone(),
            components: Vec::new(),
        })
    }
}

/// Where one kind of collection is looked for: a server per kind, or one for all, or the address.
fn servers(found: &Move, mailbox: &MoveMailbox, host: &str, kind: DavKind) -> Option<Option<String>> {
    if !mailbox.dav_url.is_empty() {
        return Some(Some(mailbox.dav_url.clone()));
    }
    let dav_host = if found.dav_host.is_empty() { host } else { found.dav_host.as_str() };
    let preset =
        |domain: &str| client::provider_of(domain).and_then(|provider| provider.start(kind)).map(str::to_owned);
    match found.dav_mode {
        DavMode::None => None,
        DavMode::Auto => Some(None),
        DavMode::Custom => Some(Some(found.dav_url.clone())),
        DavMode::Nextcloud => Some(Some(format!("https://{dav_host}/remote.php/dav/"))),
        DavMode::Sogo => Some(Some(format!("https://{dav_host}/SOGo/dav/"))),
        DavMode::Icloud => Some(preset("icloud.com")),
        DavMode::Gmx => Some(preset("gmx.net")),
        DavMode::Webde => Some(preset("web.de")),
    }
}

/// Finds the collections of the old mailbox: as the move says, and for "auto" from the address
/// first, then at the IMAP server's `/.well-known/` addresses.
async fn find(
    remote: &mut Remote<'_>,
    dns: Option<&DnsChecker>,
    found: &Move,
    mailbox: &MoveMailbox,
    kinds: &[DavKind],
) -> Result<Vec<RemoteCollection>, RemoteError> {
    let host = if mailbox.imap_host.is_empty() { found.imap_host.as_str() } else { mailbox.imap_host.as_str() };
    let mut collections = Vec::new();
    for kind in kinds {
        let Some(server) = servers(found, mailbox, host, *kind) else { continue };
        let found_here = match client::discover(remote, dns, &mailbox.old_address, server.as_deref(), &[*kind]).await {
            Err(
                RemoteError::NotFound | RemoteError::Unreachable | RemoteError::Status(_) | RemoteError::NotAllowed(_),
            ) if server.is_none() => client::discover(remote, dns, &mailbox.old_address, Some(host), &[*kind]).await,
            other => other,
        }?;
        collections.extend(found_here.collections);
    }
    Ok(collections)
}

/// Brings the contacts and calendars of one mailbox over. How many came, an error code (empty
/// when all went well) and the collections to keep for the next time.
async fn pull_dav(
    store: &Store,
    env: &Env,
    found: &Move,
    mailbox: &MoveMailbox,
    password: &str,
) -> (i64, i64, String, Option<String>) {
    let mut kinds = Vec::new();
    if found.calendars {
        kinds.push(DavKind::Calendar);
    }
    if found.contacts {
        kinds.push(DavKind::Addressbook);
    }
    if kinds.is_empty() || (found.dav_mode == DavMode::None && mailbox.dav_url.is_empty()) {
        return (0, 0, String::new(), None);
    }
    let mut remote = Remote::with_login(env.transport.as_ref(), &mailbox.login, password);
    let kept: Vec<Kept> = serde_json::from_str(&mailbox.dav_found).unwrap_or_default();
    let (collections, newly_found) = if kept.is_empty() {
        match find(&mut remote, env.dns.as_deref(), found, mailbox, &kinds).await {
            Ok(collections) => (collections, true),
            Err(err) => return (0, 0, err.code().to_owned(), None),
        }
    } else {
        (kept.iter().filter_map(Kept::collection).filter(|c| kinds.contains(&c.kind)).collect(), false)
    };
    let mut budget = client::MAX_IMPORT_BYTES;
    let (mut contacts, mut events, mut error) = (0, 0, String::new());
    for collection in &collections {
        let texts = match client::fetch_collection(&mut remote.fork(), collection, &mut budget).await {
            Ok(texts) => texts,
            Err(err) => {
                error = err.code().to_owned();
                if matches!(err, RemoteError::WrongPassword | RemoteError::RedirectedElsewhere) {
                    break;
                }
                continue;
            }
        };
        let new = NewImportCollection {
            name: collection.name.clone(),
            description: collection.description.clone(),
            color: collection.color.clone(),
        };
        match import(store, mailbox.account_id, collection.kind, new, &env.names, texts.concat()).await {
            Ok(count) if collection.kind == DavKind::Addressbook => contacts += count,
            Ok(count) => events += count,
            Err(err) => {
                tracing::warn!(%err, mailbox = %mailbox.address, "storing moved contacts or calendars failed");
                error = "failed".to_owned();
            }
        }
    }
    if collections.is_empty() && error.is_empty() {
        error = RemoteError::NotFound.code().to_owned();
    }
    let kept = newly_found
        .then(|| serde_json::to_string(&collections.iter().map(Kept::of).collect::<Vec<_>>()).ok())
        .flatten();
    (contacts, events, error, kept)
}

#[cfg(test)]
mod tests;
