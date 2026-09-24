//! Emptying mailboxes at other providers into someone's mailbox here.
//!
//! Every few minutes each fetch account is asked what is new, and every message found is handed to
//! the same pipeline that mail from other servers goes through. It is judged here, by this server's
//! filter, and lands in the inbox or in Junk accordingly -- what the provider thought of it is one
//! rule among many, see `uwumail-smtp/src/fetched.rs`.
//!
//! The IMAP client is the one the migration import uses (`import::imap`), which already speaks
//! enough of the protocol for this: connect, log in, list folders, walk UIDs.
//!
//! Three things a run is careful about:
//!
//! * **A message is taken once.** A folder remembers the last UID it read; a provider that
//!   renumbers its folders starts over, and then the message's own name keeps it from arriving
//!   twice.
//! * **Nothing is touched at the provider before this server has decided.** A message is marked
//!   or deleted there once it has arrived here, or once the filter has refused it for good -- a
//!   refused message is finished with, and keeping it would only fill a mailbox nobody reads. An
//!   answer of "later" -- greylisting, a full mailbox -- is not a decision: it leaves the message
//!   where it is and stops the folder there, so the next run offers it again. That is exactly what
//!   greylisting asks of a sending server, and here this server is the sending server. Mail this
//!   server has nowhere to put is left alone too: that is a mistake on this side, not a verdict.
//! * **New mail starts where the folders stood at the first run.** What was already there comes
//!   only when it is asked for, and then the way the migration import copies a mailbox: with its
//!   own date, not judged again, and never twice -- see [`take_backlog`].

use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use tokio::sync::watch;
use uwumail_smtp::{FetchedMailbox, Smtp, Taken};
use uwumail_store::{
    AfterFetch, BlobHash, FetchAccount, FetchFolder, FetchSecurity, IngestRequest, MailboxRole, MailboxTarget, Store,
    StoreError,
};

use crate::import::imap::{Connection, Fetched, Source, Token, folders, parse_fetch, quoted};

/// How often the list of fetch accounts is looked at. Each account has its own interval on top.
const TICK: Duration = Duration::from_secs(30);
/// At most this many messages from one folder per run, so a mailbox with a backlog is worked
/// through in portions instead of holding the connection for an hour.
const PER_RUN: usize = 200;
/// How many messages are asked for at once.
const BATCH: usize = 20;
/// How long one account's run may take before it is cut short and continued next time.
const RUN_LIMIT: Duration = Duration::from_secs(300);

/// Where to really connect instead of to the provider named in the fetch account. Always `None`
/// outside tests, where the provider is a server of ours on localhost with a certificate it made
/// itself, and neither its name nor its port can be looked up.
#[derive(Debug, Clone)]
pub(crate) struct Detour {
    pub address: String,
    pub tls_name: String,
    pub roots: rustls::RootCertStore,
}

/// Fetches from every mailbox whose turn it is, until `shutdown` changes.
pub async fn run_fetchers(
    store: Store,
    smtp: Smtp,
    egress: uwumail_smtp::egress::Egress,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let due = match store.fetch_accounts_due().await {
            Ok(due) => due,
            Err(err) => {
                tracing::warn!(%err, "reading the fetch accounts failed");
                Vec::new()
            }
        };
        for account in due {
            if *shutdown.borrow() {
                return;
            }
            let address = account.address.clone();
            let id = account.id;
            let owner = account.account_id;
            let backlog = account.backlog_at.is_some();
            let run = tokio::time::timeout(
                RUN_LIMIT,
                run_once(&store, &smtp, account, None, Some(egress.dialer(uwumail_smtp::egress::Purpose::Fetch))),
            );
            let (fetched, error) = match run.await {
                Ok(Ok(fetched)) => (fetched, None),
                Ok(Err(err)) => {
                    tracing::warn!(%address, err = %format!("{err:#}"), "fetching mail failed");
                    (0, Some(format!("{err:#}")))
                }
                Err(_) => {
                    tracing::warn!(%address, "fetching mail took too long and was cut short");
                    (0, Some("the provider took too long to answer".to_owned()))
                }
            };
            let went_well = error.is_none();
            if let Err(err) = store.note_fetch_run(id, fetched, error).await {
                tracing::warn!(%address, %err, "writing down how a fetch run went failed");
            }
            // While the mail that was already there is still coming, the next portion follows on
            // the next tick instead of waiting out the interval. Only after a run that went well: a
            // provider that is down is not asked every thirty seconds.
            if backlog
                && went_well
                && matches!(store.fetch_account(owner, id).await, Ok(Some(account)) if account.backlog_at.is_some())
            {
                let _ = store.fetch_account_due_now(owner, id).await;
            }
        }
        if let Err(err) = store.prune_fetch_seen().await {
            tracing::debug!(%err, "forgetting old fetched messages failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// Where a message goes and what is known about where it came from.
fn mailbox_of(account: &FetchAccount) -> FetchedMailbox {
    FetchedMailbox {
        id: account.id,
        account_id: account.account_id,
        address: account.address.clone(),
        host: account.host.clone(),
        auth_serv_id: account.auth_serv_id.clone(),
    }
}

/// The value of a status code like `[UIDVALIDITY 7]` in the answers to a command.
fn status(responses: &[crate::import::imap::Response], code: &str) -> Option<u32> {
    responses.iter().find_map(|response| {
        let rest = response.text.split_once(&format!("[{code} "))?.1;
        rest.split(']').next()?.trim().parse::<u32>().ok()
    })
}

/// What a message is remembered by, so it is not brought twice: its own name where it has one,
/// otherwise the bytes it is made of.
fn message_key(raw: &[u8]) -> String {
    match uwumail_smtp::header_value(raw, "Message-ID") {
        Some(id) if !id.is_empty() && id.len() <= 200 => id,
        _ => format!("bytes:{}", uwumail_store::BlobHash::of(raw).as_str()),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// Fetches from one mailbox and returns how many messages it brought.
async fn run_once(
    store: &Store,
    smtp: &Smtp,
    account: FetchAccount,
    detour: Option<Detour>,
    dialer: Option<uwumail_smtp::egress::Dialer>,
) -> anyhow::Result<i64> {
    if account.security == FetchSecurity::Starttls {
        bail!("this server fetches over TLS only, so far -- use the provider's TLS port, usually 993");
    }
    if detour.is_none() {
        // The host is re-checked here so a public-looking name cannot resolve to this machine or the
        // local network (security-audit-0.5.2 S-10). The detour is used only by tests against a
        // local server, and is exempt.
        let public = tokio::net::lookup_host((account.host.as_str(), account.port))
            .await
            .map(|addrs| addrs.into_iter().any(|addr| uwumail_smtp::is_public(addr.ip())))
            .unwrap_or(false);
        if !public {
            bail!("{} does not resolve to a public address", account.host);
        }
    }
    let password = store
        .fetch_password(account.account_id, account.id)
        .await?
        .ok_or_else(|| anyhow!("the password for {} is gone", account.address))?;
    let to = store
        .account_by_id(account.account_id)
        .await?
        .ok_or_else(|| anyhow!("the mailbox this fetches into does not exist any more"))?
        .login;

    let source = match detour {
        None => Source {
            address: format!("{}:{}", account.host, account.port),
            tls_name: Some(account.host.clone()),
            roots: None,
            master_user: None,
            password,
            // Only through the proxy when the admin wants fetching to take it; straight otherwise.
            dialer: dialer.filter(|dialer| dialer.proxied()),
        },
        Some(detour) => Source {
            address: detour.address,
            tls_name: Some(detour.tls_name),
            roots: Some(detour.roots),
            master_user: None,
            password,
            dialer: None,
        },
    };
    let mut connection = Connection::open(&source).await?;
    connection
        .command(&format!("LOGIN {} {}", quoted(&account.username), quoted(&source.password)))
        .await
        .context("the provider did not accept the user name and password")?;

    // The inbox always; the provider's junk folder when it was asked for, because this server wants
    // to judge that mail itself rather than take the provider's word for it.
    let all = folders(&mut connection).await?;
    let wanted: Vec<_> = all
        .into_iter()
        .filter(|folder| match folder.role {
            Some(MailboxRole::Inbox) => true,
            Some(MailboxRole::Junk) => account.fetch_junk,
            _ => folder.raw.eq_ignore_ascii_case("INBOX"),
        })
        .collect();
    if wanted.is_empty() {
        bail!("the provider offers no inbox to fetch from");
    }

    let mut fetched = 0;
    let mut backlog_done = true;
    for folder in wanted {
        let from_junk = folder.role == Some(MailboxRole::Junk);
        let (taken, done) = take_folder(store, smtp, &account, &to, &mut connection, &folder.raw, from_junk).await?;
        fetched += taken;
        backlog_done &= done;
    }
    let _ = connection.command("LOGOUT").await;
    // The mail that was already there counts as brought once every folder is through with it.
    if let Some(at) = account.backlog_at
        && backlog_done
    {
        store.finish_fetch_backlog(account.id, at).await?;
        tracing::info!(address = %account.address, "the mail that was already there is all here");
    }
    Ok(fetched)
}

/// Works on one folder: first what is new, then -- when it was asked for -- a portion of the mail
/// that was already there. Returns what it brought, and whether the folder is through with the
/// mail that was already there (always, when none was asked for).
async fn take_folder(
    store: &Store,
    smtp: &Smtp,
    account: &FetchAccount,
    to: &str,
    connection: &mut Connection,
    folder: &str,
    from_junk: bool,
) -> anyhow::Result<(i64, bool)> {
    let responses = connection.command(&format!("SELECT {}", quoted(folder))).await?;
    let uid_validity = status(&responses, "UIDVALIDITY").unwrap_or(0) as i64;
    let uid_next = status(&responses, "UIDNEXT").unwrap_or(1) as i64;

    let known = store.fetch_folder(account.id, folder.to_owned()).await?;
    let (mut state, first) = match known {
        // The provider renumbered the folder: its UIDs say nothing about ours any more, so the
        // count starts again and the remembered message names keep the old ones from coming back.
        Some(known) if known.uid_validity != uid_validity => {
            tracing::info!(address = %account.address, folder, "the provider renumbered this folder");
            (FetchFolder { uid_validity, ..Default::default() }, false)
        }
        Some(known) => (known, false),
        // The first time this folder is seen, new mail starts where it stands now. What is already
        // there only comes when it is asked for, below.
        None => {
            let state = FetchFolder { uid_validity, last_uid: (uid_next - 1).max(0), ..Default::default() };
            tracing::info!(address = %account.address, folder, from = state.last_uid, "fetching starts here");
            (state, true)
        }
    };
    // Asked for the mail that was already there: everything up to where new mail begins belongs to
    // it. A later request starts the folder over; what arrived meanwhile is recognised and skipped.
    if let Some(at) = account.backlog_at
        && state.backlog_at != Some(at)
    {
        state.backlog_at = Some(at);
        state.backlog_next = 1;
        state.backlog_until = Some(state.last_uid);
    }

    let mut taken = 0;
    if !first {
        taken += take_new(store, smtp, account, to, connection, folder, from_junk, &mut state).await?;
    }
    taken += take_backlog(store, account, connection, folder, from_junk, &mut state).await?;
    store.set_fetch_folder(account.id, folder.to_owned(), state).await?;
    if taken > 0 {
        tracing::info!(address = %account.address, folder, taken, "fetched mail");
    }
    let done = account.backlog_at.is_none_or(|at| state.backlog_at == Some(at) && state.backlog_until.is_none());
    Ok((taken, done))
}

/// Takes what arrived in a folder since the last run. Stops early when the server asks for a
/// message later, so the folder keeps its order and nothing is skipped.
#[allow(clippy::too_many_arguments)]
async fn take_new(
    store: &Store,
    smtp: &Smtp,
    account: &FetchAccount,
    to: &str,
    connection: &mut Connection,
    folder: &str,
    from_junk: bool,
    state: &mut FetchFolder,
) -> anyhow::Result<i64> {
    // A message that keeps being asked for later and never gets through would hold up its folder
    // for good, so after a day it is stepped over and the folder moves on.
    if state.hold_expired(now()) {
        if let Some(uid) = state.held_uid {
            tracing::warn!(address = %account.address, folder, uid, "a message could not be taken for a day, moving on");
            state.last_uid = state.last_uid.max(uid);
        }
        state.held_uid = None;
        state.held_since = None;
    }
    let waited_for = state.held_uid;

    let uids: Vec<u32> = connection
        .command(&format!("UID SEARCH UID {}:*", state.last_uid + 1))
        .await?
        .iter()
        .filter(|response| response.tokens.get(1) == Some(&Token::Atom("SEARCH".into())))
        .flat_map(|response| response.tokens.iter().skip(2).filter_map(Token::text))
        .filter_map(|uid| uid.parse::<u32>().ok())
        .filter(|uid| i64::from(*uid) > state.last_uid)
        .take(PER_RUN)
        .collect();
    if uids.is_empty() {
        return Ok(0);
    }

    let mut taken = 0;
    let mut deleted = false;
    let mut held = None;
    'batches: for batch in uids.chunks(BATCH) {
        let range = batch.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let responses = connection.command(&format!("UID FETCH {range} (UID FLAGS INTERNALDATE BODY.PEEK[])")).await?;
        let mut messages: Vec<Fetched> = responses.iter().filter_map(parse_fetch).collect();
        messages.sort_by_key(|message| message.uid);
        // A UID the search named but the answer left out would be passed over for good once a later
        // one moves the folder on. Nothing can be done about it from here, but it is written down.
        for uid in batch {
            if !messages.iter().any(|message| message.uid == *uid && message.body.is_some()) {
                tracing::warn!(address = %account.address, folder, uid, "the provider did not hand this message out");
            }
        }
        for message in messages {
            let Some(raw) = message.body else { continue };
            let uid = i64::from(message.uid);
            let mut judged = false;
            match take_message(store, smtp, account, to, from_junk, raw).await? {
                Taken::Kept => {}
                Taken::Refused(answer) => {
                    // The filter has decided about this one, so it is finished with either way.
                    // It is not brought here, and at the provider it is dealt with like any other
                    // message the run has been through -- otherwise the mailbox fills up with
                    // exactly what this server refuses, run after run, and nobody empties it.
                    tracing::info!(address = %account.address, folder, uid, %answer, "refused, cleared at the provider");
                    judged = true;
                }
                Taken::Nowhere(answer) => {
                    // Not the message's fault: there is nowhere here to put it. Leave it
                    // untouched -- somebody else's mail is not deleted over a mistake on this
                    // side -- but step past it so the folder is not stuck on it every run.
                    tracing::warn!(address = %account.address, folder, uid, %answer, "nowhere to put it; left at the provider");
                    state.last_uid = state.last_uid.max(uid);
                    continue;
                }
                Taken::Later(answer) => {
                    tracing::info!(address = %account.address, folder, uid, %answer, "left for the next run");
                    held = Some(uid);
                    break 'batches;
                }
            }
            // Only now, with the message either here or decided about, is anything changed at the
            // provider.
            match account.after_fetch {
                AfterFetch::MarkRead => {
                    let _ = connection.command(&format!("UID STORE {uid} +FLAGS (\\Seen)")).await;
                }
                AfterFetch::Delete => {
                    if connection.command(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).await.is_ok() {
                        deleted = true;
                    }
                }
            }
            state.last_uid = state.last_uid.max(uid);
            // A refused message was cleared, not fetched: it never reached anybody's mailbox, so
            // it does not count towards what this run brought.
            if !judged {
                taken += 1;
            }
        }
    }
    if deleted {
        let _ = connection.command("EXPUNGE").await;
    }
    // The clock on a waiting message keeps running while it is the same one, so a message that can
    // never be taken is stepped over a day after it first stopped the folder, not a day after the
    // last attempt.
    match held {
        Some(uid) => {
            if waited_for != Some(uid) {
                state.held_since = Some(now());
            }
            state.held_uid = Some(uid);
        }
        None => {
            state.held_uid = None;
            state.held_since = None;
        }
    }
    Ok(taken)
}

/// Takes a portion of the mail that was already in a folder when it was set up.
///
/// This is somebody's own old mail, from a mailbox they proved is theirs by opening it, so it is
/// brought over the way the migration import brings a mailbox: not judged again by the filter --
/// that would score months-old mail against DKIM keys the senders have long rotated -- but filed
/// where the provider had it, the inbox into the inbox and the junk folder into Junk, with the date
/// it arrived there and read or flagged as it was. What is already here, by whatever way it came,
/// is recognised and not brought twice. Afterwards it is marked or deleted at the provider like
/// any other message.
async fn take_backlog(
    store: &Store,
    account: &FetchAccount,
    connection: &mut Connection,
    folder: &str,
    from_junk: bool,
    state: &mut FetchFolder,
) -> anyhow::Result<i64> {
    let Some(until) = state.backlog_until else {
        return Ok(0);
    };
    // Only into a mailbox of the person's own. An account without one has its mail redirected
    // elsewhere, and old mail is not somebody else's to receive: it stays at the provider.
    let owner = store.account_by_id(account.account_id).await?;
    if !owner.is_some_and(|owner| owner.has_mailbox()) {
        tracing::warn!(address = %account.address, folder, "no mailbox here for the mail that was already there");
        state.backlog_until = None;
        return Ok(0);
    }
    let uids: Vec<u32> = if state.backlog_next > until {
        Vec::new()
    } else {
        connection
            .command(&format!("UID SEARCH UID {}:{until}", state.backlog_next))
            .await?
            .iter()
            .filter(|response| response.tokens.get(1) == Some(&Token::Atom("SEARCH".into())))
            .flat_map(|response| response.tokens.iter().skip(2).filter_map(Token::text))
            .filter_map(|uid| uid.parse::<u32>().ok())
            .filter(|uid| (state.backlog_next..=until).contains(&i64::from(*uid)))
            .take(PER_RUN)
            .collect()
    };
    // Fewer than a full portion means this is the last one -- but the folder only counts as through
    // once all of it has really been dealt with, so a run that breaks off halfway starts here again.
    let last_portion = uids.len() < PER_RUN;
    let mailbox = MailboxTarget::Role(if from_junk { MailboxRole::Junk } else { MailboxRole::Inbox });
    let mut uids = uids;
    uids.sort_unstable();

    let mut taken = 0;
    let mut deleted = false;
    // Whether every message of this portion was dealt with.
    let result: anyhow::Result<bool> = async {
        for batch in uids.chunks(BATCH) {
            let range = batch.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let responses =
                connection.command(&format!("UID FETCH {range} (UID FLAGS INTERNALDATE BODY.PEEK[])")).await?;
            let mut by_uid: std::collections::HashMap<u32, Fetched> =
                responses.iter().filter_map(parse_fetch).map(|fetched| (fetched.uid, fetched)).collect();
            for uid in batch {
                let fetched = by_uid.remove(uid);
                let gone = fetched
                    .as_ref()
                    .is_some_and(|fetched| fetched.flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Deleted")));
                match fetched {
                    Some(Fetched { body: Some(raw), flags, internal_date, .. }) if !gone => {
                        let key = message_key(&raw);
                        let message_id = uwumail_smtp::header_value(&raw, "Message-ID");
                        let here = store.is_fetch_seen(account.id, key.clone()).await?
                            || store.holds_message(account.account_id, message_id, BlobHash::of(&raw)).await?;
                        if !here {
                            let keywords =
                                flags.iter().filter_map(|flag| uwumail_imap::parser::keyword_of_flag(flag)).collect();
                            let request = IngestRequest {
                                account_id: account.account_id,
                                raw,
                                mailboxes: vec![mailbox],
                                keywords,
                                received_at: internal_date,
                            };
                            match store.ingest(request).await {
                                Ok(_) => taken += 1,
                                // No room here: it waits at the provider, untouched, and the next
                                // run carries on from this very message.
                                Err(StoreError::QuotaExceeded) => {
                                    tracing::info!(address = %account.address, folder, uid, "no room for the mail that was already there");
                                    return Ok(false);
                                }
                                Err(err) => return Err(err.into()),
                            }
                            store.mark_fetch_seen(account.id, key).await?;
                        }
                        match account.after_fetch {
                            AfterFetch::MarkRead => {
                                let _ = connection.command(&format!("UID STORE {uid} +FLAGS (\\Seen)")).await;
                            }
                            AfterFetch::Delete => {
                                if connection.command(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).await.is_ok() {
                                    deleted = true;
                                }
                            }
                        }
                    }
                    Some(_) if gone => {}
                    // The provider named it in its search and then did not hand it out. Nothing
                    // can be done about it from here, but it should not pass unnoticed.
                    _ => tracing::warn!(address = %account.address, folder, uid, "the provider did not hand this message out"),
                }
                state.backlog_next = i64::from(*uid) + 1;
            }
            // Progress survives a run that is cut short.
            store.set_fetch_folder(account.id, folder.to_owned(), *state).await?;
        }
        Ok(true)
    }
    .await;
    if deleted {
        let _ = connection.command("EXPUNGE").await;
    }
    if result? && last_portion {
        state.backlog_until = None;
    }
    Ok(taken)
}

/// Hands one message over, unless this mailbox brought it before.
async fn take_message(
    store: &Store,
    smtp: &Smtp,
    account: &FetchAccount,
    to: &str,
    from_junk: bool,
    raw: Vec<u8>,
) -> anyhow::Result<Taken> {
    let key = message_key(&raw);
    if store.is_fetch_seen(account.id, key.clone()).await? {
        tracing::debug!(address = %account.address, "this message was already here");
        return Ok(Taken::Kept);
    }
    let taken = uwumail_smtp::deliver_fetched(smtp, mailbox_of(account), from_junk, to.to_owned(), raw).await;
    // Only a message that was really taken is remembered as seen. One left for later (greylisting,
    // a transient store error) stays unseen, so the next run offers it again instead of skipping it.
    if matches!(taken, Taken::Kept) {
        store.mark_fetch_seen(account.id, key).await?;
    }
    Ok(taken)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uwumail_smtp::{DeliveryConfig, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig};
    use uwumail_store::{AccountUpdate, IngestRequest, MailboxTarget, NewAccount, NewFetchAccount, Role};

    use super::*;

    const PASSWORD: &str = "katzenpfote-123";

    async fn store_with_person(path: &std::path::Path, password: Option<&str>) -> (Store, i64) {
        let store = Store::open(path).await.unwrap();
        store.create_domain("example.de").await.unwrap();
        let new = NewAccount {
            address: "mini@example.de".into(),
            display_name: "Mini".into(),
            password: password.map(str::to_owned),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(new).await.unwrap().id;
        (store, id)
    }

    /// Puts a message into a folder of the provider's mailbox.
    async fn at_provider(store: &Store, account: i64, mailbox: MailboxTarget, subject: &str) {
        let raw = format!(
            "From: shop@shop.example\r\nTo: mini@freemail.example\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@shop.example>\r\n\r\nHallo\r\n"
        );
        at_provider_raw(store, account, mailbox, raw.into_bytes()).await;
    }

    /// The same, for a message whose headers the test writes itself.
    async fn at_provider_raw(store: &Store, account: i64, mailbox: MailboxTarget, raw: Vec<u8>) {
        store
            .ingest(IngestRequest {
                account_id: account,
                raw,
                mailboxes: vec![mailbox],
                keywords: vec![],
                received_at: Some(1_700_000_000),
            })
            .await
            .unwrap();
    }

    /// Our own server, with the filter off: what it makes of a message is tested in the smtp crate,
    /// here it is only about what a run takes and what it leaves behind.
    fn our_smtp(store: Store) -> Smtp {
        Smtp::new(
            store,
            SmtpSettings {
                hostname: "mail.example.de".into(),
                smtp: SmtpConfig { verify_senders: false, ..Default::default() },
                spam: SpamConfig { enabled: false, ..Default::default() },
                delivery: DeliveryConfig::default(),
                tone: ToneConfig::default(),
                server_tls: None,
            },
        )
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_takes_what_is_new_once_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        // The provider: an IMAP server of ours, standing in for the other side.
        let (provider, provider_id) = store_with_person(&dir.path().join("provider"), Some(PASSWORD)).await;
        at_provider(&provider, provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Alt").await;

        let generated = rcgen::generate_simple_self_signed(vec!["imap.freemail.example".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(uwumail_imap::Imap::new(provider.clone(), 1 << 20).serve(listener, Arc::new(tls), shutdown_rx));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();

        // Our side, with a mailbox that fetches from there.
        let (ours, our_id) = store_with_person(&dir.path().join("ours"), None).await;
        let smtp = our_smtp(ours.clone());
        let fetched = ours
            .create_fetch_account(NewFetchAccount {
                account_id: our_id,
                address: "mini@freemail.example".into(),
                host: "imap.freemail.example".into(),
                port: 993,
                security: FetchSecurity::Tls,
                // The provider's server is ours, so the login is the one it knows.
                username: "mini@example.de".into(),
                password: PASSWORD.into(),
                after_fetch: AfterFetch::MarkRead,
                fetch_junk: true,
                interval_secs: uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
                auth_serv_id: String::new(),
            })
            .await
            .unwrap();
        // Nothing looks up imap.freemail.example: the run is sent to the port above instead, and the
        // certificate is still checked against the name the fetch account names.
        let detour = Detour { address: format!("127.0.0.1:{port}"), tls_name: "imap.freemail.example".into(), roots };
        let run = || {
            let (store, smtp, detour) = (ours.clone(), smtp.clone(), detour.clone());
            async move {
                let account = store.fetch_account(our_id, fetched.id).await.unwrap().unwrap();
                run_once(&store, &smtp, account, Some(detour), None).await.unwrap()
            }
        };

        assert_eq!(run().await, 0, "the first run only writes down where the folders stand");
        let inbox = |store: &Store, id: i64| {
            let store = store.clone();
            async move {
                let mailboxes = store.mailboxes(id).await.unwrap();
                let inbox = mailboxes.into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
                (inbox.total_emails, inbox.unread_emails)
            }
        };
        assert_eq!(inbox(&ours, our_id).await, (0, 0), "and brings nothing with it");

        at_provider(&provider, provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Neu").await;
        assert_eq!(run().await, 1, "what arrived since comes over");
        assert_eq!(inbox(&ours, our_id).await, (1, 1), "and lands in the inbox");
        assert_eq!(run().await, 0, "and is not taken a second time");

        // It was marked as read at the provider, and nothing was deleted there.
        assert_eq!(inbox(&provider, provider_id).await, (2, 1), "only the fetched one is read there now");
    }

    /// A provider that is an IMAP server of ours, and our own side with a mailbox that fetches from
    /// it set to delete what it has dealt with -- the setting under test in the two tests below.
    struct Rig {
        _dir: tempfile::TempDir,
        _shutdown: tokio::sync::watch::Sender<bool>,
        provider: Store,
        provider_id: i64,
        ours: Store,
        our_id: i64,
        smtp: Smtp,
        fetch_id: i64,
        detour: Detour,
    }

    impl Rig {
        async fn new(after_fetch: AfterFetch) -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let (provider, provider_id) = store_with_person(&dir.path().join("provider"), Some(PASSWORD)).await;
            let generated = rcgen::generate_simple_self_signed(vec!["imap.freemail.example".to_owned()]).unwrap();
            let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
            let tls =
                rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_no_client_auth()
                    .with_single_cert(vec![generated.cert.der().clone()], key)
                    .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(uwumail_imap::Imap::new(provider.clone(), 1 << 20).serve(
                listener,
                Arc::new(tls),
                shutdown_rx,
            ));
            let mut roots = rustls::RootCertStore::empty();
            roots.add(generated.cert.der().clone()).unwrap();

            let (ours, our_id) = store_with_person(&dir.path().join("ours"), None).await;
            let smtp = our_smtp(ours.clone());
            let fetch_id = ours
                .create_fetch_account(NewFetchAccount {
                    account_id: our_id,
                    address: "mini@freemail.example".into(),
                    host: "imap.freemail.example".into(),
                    port: 993,
                    security: FetchSecurity::Tls,
                    username: "mini@example.de".into(),
                    password: PASSWORD.into(),
                    after_fetch,
                    fetch_junk: false,
                    interval_secs: uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
                    auth_serv_id: String::new(),
                })
                .await
                .unwrap()
                .id;
            let detour =
                Detour { address: format!("127.0.0.1:{port}"), tls_name: "imap.freemail.example".into(), roots };
            Rig { _dir: dir, _shutdown: shutdown, provider, provider_id, ours, our_id, smtp, fetch_id, detour }
        }

        async fn run(&self) -> i64 {
            let account = self.ours.fetch_account(self.our_id, self.fetch_id).await.unwrap().unwrap();
            run_once(&self.ours, &self.smtp, account, Some(self.detour.clone()), None).await.unwrap()
        }

        async fn inbox(store: &Store, id: i64) -> i64 {
            let mailboxes = store.mailboxes(id).await.unwrap();
            mailboxes.into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap().total_emails
        }

        async fn at_provider(&self) -> i64 {
            Rig::inbox(&self.provider, self.provider_id).await
        }

        async fn here(&self) -> i64 {
            Rig::inbox(&self.ours, self.our_id).await
        }
    }

    /// What a refused message leaves behind at the provider. This server has judged it, so it is
    /// finished with it either way: it is cleared there like any other, because a mailbox that
    /// keeps everything this server refuses is a mailbox that fills up and that nobody empties.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_message_is_cleared_at_the_provider() {
        let rig = Rig::new(AfterFetch::Delete).await;
        assert_eq!(rig.run().await, 0, "the first run only writes down where the folders stand");

        // A mail loop: refused before the filter even runs, the same `Taken::Refused` a virus, a
        // blocked sender or a DMARC policy that rejects produces further down the same path.
        let mut raw = String::from("From: shop@shop.example\r\nTo: mini@freemail.example\r\n");
        for hop in 0..=51 {
            raw.push_str(&format!(
                "Received: from a{hop}.example by b{hop}.example; Mon, 1 Jan 2026 00:00:00 +0000\r\n"
            ));
        }
        raw.push_str("Subject: Schleife\r\nMessage-ID: <loop@shop.example>\r\n\r\nHallo\r\n");
        at_provider_raw(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), raw.into_bytes())
            .await;
        assert_eq!(rig.at_provider().await, 1, "it is lying at the provider");

        assert_eq!(rig.run().await, 0, "a refused message is not counted as fetched");
        assert_eq!(rig.here().await, 0, "and nothing of it arrived here");
        assert_eq!(rig.at_provider().await, 0, "but it was cleared away at the provider");
        assert_eq!(rig.run().await, 0, "and it is not offered again");
    }

    /// The case that must never go the way a refusal goes. A full mailbox here says nothing about
    /// the message: its owner makes room and then it can come. Were it read as a refusal, it would
    /// be cleared at the provider like one -- and a mailbox that ran out of room here would quietly
    /// delete everything arriving there. So it is "later": left where it is, the folder held on it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_mailbox_here_never_costs_the_mail_at_the_provider() {
        let rig = Rig::new(AfterFetch::Delete).await;
        assert_eq!(rig.run().await, 0, "the first run only writes down where the folders stand");

        // One byte of room: anything at all fills it.
        rig.ours
            .update_account("mini@example.de", AccountUpdate { quota_bytes: Some(1), ..Default::default() })
            .await
            .unwrap();
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Wichtig").await;
        assert_eq!(rig.at_provider().await, 1, "it is lying at the provider");

        assert_eq!(rig.run().await, 0, "nothing could be taken");
        assert_eq!(rig.here().await, 0, "because there is no room here");
        assert_eq!(rig.at_provider().await, 1, "and it is still at the provider, although it is set to delete");

        // Room again: the same message comes, and only now is it deleted there.
        rig.ours
            .update_account("mini@example.de", AccountUpdate { quota_bytes: Some(0), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(rig.run().await, 1, "with room it comes over");
        assert_eq!(rig.here().await, 1);
        assert_eq!(rig.at_provider().await, 0, "and is cleared there once it is really here");
    }

    /// Puts a message at the provider that the person had already read there.
    async fn read_at_provider(store: &Store, account: i64, subject: &str) {
        let raw = format!(
            "From: shop@shop.example\r\nTo: mini@freemail.example\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@shop.example>\r\n\r\nHallo\r\n"
        );
        store
            .ingest(IngestRequest {
                account_id: account,
                raw: raw.into_bytes(),
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec!["$seen".into()],
                received_at: Some(1_700_000_000),
            })
            .await
            .unwrap();
    }

    /// What is in somebody's inbox here: subject, date and whether it was read.
    async fn inbox_mail(store: &Store, id: i64) -> Vec<(String, i64, bool)> {
        let mailboxes = store.mailboxes(id).await.unwrap();
        let inbox = mailboxes.into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
        let uids = store.imap_messages(id, inbox.id).await.unwrap().messages.iter().map(|m| m.uid).collect();
        let mut mail: Vec<_> = store
            .imap_emails(id, inbox.id, uids)
            .await
            .unwrap()
            .into_iter()
            .map(|email| (email.subject, email.received_at, email.keywords.iter().any(|k| k == "$seen")))
            .collect();
        mail.sort();
        mail
    }

    /// The mail that was already there when the mailbox was set up comes once it is asked for:
    /// with the date it arrived at the provider and read as it was there, not as a heap of unread
    /// mail from today. Afterwards it is dealt with at the provider like any other.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_mail_that_was_there_comes_when_asked_for_with_its_own_date() {
        let rig = Rig::new(AfterFetch::Delete).await;
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Alt").await;
        read_at_provider(&rig.provider, rig.provider_id, "Gelesen").await;

        assert_eq!(rig.run().await, 0, "the first run only writes down where the folders stand");
        assert_eq!(rig.here().await, 0, "and leaves what was there where it is");
        assert_eq!(rig.at_provider().await, 2);

        rig.ours.request_fetch_backlog(rig.our_id, rig.fetch_id).await.unwrap();
        assert_eq!(rig.run().await, 2, "asked for, it comes over");
        assert_eq!(
            inbox_mail(&rig.ours, rig.our_id).await,
            [("Alt".to_owned(), 1_700_000_000, false), ("Gelesen".to_owned(), 1_700_000_000, true)],
            "with the date it had at the provider, and read where it was read there"
        );
        assert_eq!(rig.at_provider().await, 0, "and is cleared there, the mailbox being set to delete");
        let account = rig.ours.fetch_account(rig.our_id, rig.fetch_id).await.unwrap().unwrap();
        assert_eq!(account.backlog_at, None, "it is all here, so nothing is waiting any more");

        assert_eq!(rig.run().await, 0, "and nothing comes a second time");
        assert_eq!(rig.here().await, 2);
    }

    /// A mailbox that has been fetching for a while and is set to mark as read still has the mail
    /// it already brought. Asking for what was there must bring only what is missing -- whether the
    /// rest came by fetching, or by some other way this fetch account never saw.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_is_already_here_is_not_brought_twice() {
        let rig = Rig::new(AfterFetch::MarkRead).await;
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Alt").await;
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Direkt").await;
        // The same message reached this mailbox another way too -- sent here as well, imported, or
        // fetched so long ago that the fetch account no longer remembers it. Only the mailbox knows.
        at_provider(&rig.ours, rig.our_id, MailboxTarget::Role(MailboxRole::Inbox), "Direkt").await;
        assert_eq!(rig.run().await, 0, "the first run only writes down where the folders stand");
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Neu").await;
        assert_eq!(rig.run().await, 1, "what arrived since comes the usual way");

        rig.ours.request_fetch_backlog(rig.our_id, rig.fetch_id).await.unwrap();
        assert_eq!(rig.run().await, 1, "of the three still at the provider, only the old one is new here");
        let subjects: Vec<String> = inbox_mail(&rig.ours, rig.our_id).await.into_iter().map(|(s, ..)| s).collect();
        assert_eq!(subjects, ["Alt", "Direkt", "Neu"], "each of them once");
        assert_eq!(rig.at_provider().await, 3, "marked as read, all are still at the provider");
    }

    /// Not enough room here for the old mail: it waits at the provider, untouched, and the run
    /// that finds room again carries on where the last one stopped.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_mail_that_was_there_waits_for_room_like_new_mail() {
        let rig = Rig::new(AfterFetch::Delete).await;
        at_provider(&rig.provider, rig.provider_id, MailboxTarget::Role(MailboxRole::Inbox), "Alt").await;
        assert_eq!(rig.run().await, 0, "the first run only writes down where the folders stand");

        rig.ours
            .update_account("mini@example.de", AccountUpdate { quota_bytes: Some(1), ..Default::default() })
            .await
            .unwrap();
        rig.ours.request_fetch_backlog(rig.our_id, rig.fetch_id).await.unwrap();
        assert_eq!(rig.run().await, 0, "there is no room for it");
        assert_eq!(rig.at_provider().await, 1, "so it is still at the provider, although it is set to delete");
        let waiting = rig.ours.fetch_account(rig.our_id, rig.fetch_id).await.unwrap().unwrap();
        assert!(waiting.backlog_at.is_some(), "and it is still waiting to come");

        rig.ours
            .update_account("mini@example.de", AccountUpdate { quota_bytes: Some(0), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(rig.run().await, 1, "with room it comes");
        assert_eq!(rig.at_provider().await, 0);
        let done = rig.ours.fetch_account(rig.our_id, rig.fetch_id).await.unwrap().unwrap();
        assert_eq!(done.backlog_at, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_that_was_here_before_is_not_brought_again() {
        let dir = tempfile::tempdir().unwrap();
        let (ours, our_id) = store_with_person(dir.path(), None).await;
        let fetched = ours
            .create_fetch_account(NewFetchAccount {
                account_id: our_id,
                address: "mini@freemail.example".into(),
                host: "imap.freemail.example".into(),
                port: 993,
                security: FetchSecurity::Tls,
                username: "mini@freemail.example".into(),
                password: PASSWORD.into(),
                after_fetch: AfterFetch::MarkRead,
                fetch_junk: false,
                interval_secs: uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
                auth_serv_id: String::new(),
            })
            .await
            .unwrap();
        let raw = b"Message-ID: <one@shop.example>\r\nSubject: eins\r\n\r\nHallo\r\n";
        assert_eq!(message_key(raw), "<one@shop.example>");
        assert!(!ours.mark_fetch_seen(fetched.id, message_key(raw)).await.unwrap());
        assert!(ours.mark_fetch_seen(fetched.id, message_key(raw)).await.unwrap(), "the second time it is known");

        // Without a name of its own, the bytes stand in for it.
        let unnamed = b"Subject: ohne Namen\r\n\r\nHallo\r\n";
        assert!(message_key(unnamed).starts_with("bytes:"));
    }
}
