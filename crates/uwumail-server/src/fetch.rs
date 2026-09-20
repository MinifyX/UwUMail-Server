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
//! * **A message is never lost.** It is only marked or deleted at the provider once this server has
//!   really taken it. An answer of "later" -- greylisting, a full mailbox -- leaves it where it is
//!   and stops the folder there, so the next run offers it again. That is exactly what greylisting
//!   asks of a sending server, and here this server is the sending server.
//! * **The first run takes nothing.** It only writes down where the folders stand. Years of old
//!   mail would otherwise arrive as if it came today; `uwumail-server import imap` copies a
//!   mailbox over with its folders and dates for that.

use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use tokio::sync::watch;
use uwumail_smtp::{FetchedMailbox, Smtp, Taken};
use uwumail_store::{AfterFetch, FetchAccount, FetchFolder, FetchSecurity, MailboxRole, Store};

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
pub async fn run_fetchers(store: Store, smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
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
            let run = tokio::time::timeout(RUN_LIMIT, run_once(&store, &smtp, account, None));
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
            if let Err(err) = store.note_fetch_run(id, fetched, error).await {
                tracing::warn!(%address, %err, "writing down how a fetch run went failed");
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
async fn run_once(store: &Store, smtp: &Smtp, account: FetchAccount, detour: Option<Detour>) -> anyhow::Result<i64> {
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
        },
        Some(detour) => Source {
            address: detour.address,
            tls_name: Some(detour.tls_name),
            roots: Some(detour.roots),
            master_user: None,
            password,
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
    for folder in wanted {
        let from_junk = folder.role == Some(MailboxRole::Junk);
        fetched += take_folder(store, smtp, &account, &to, &mut connection, &folder.raw, from_junk).await?;
    }
    let _ = connection.command("LOGOUT").await;
    Ok(fetched)
}

/// Takes what is new in one folder. Stops early when the server asks for a message later, so the
/// folder keeps its order and nothing is skipped.
async fn take_folder(
    store: &Store,
    smtp: &Smtp,
    account: &FetchAccount,
    to: &str,
    connection: &mut Connection,
    folder: &str,
    from_junk: bool,
) -> anyhow::Result<i64> {
    let responses = connection.command(&format!("SELECT {}", quoted(folder))).await?;
    let uid_validity = status(&responses, "UIDVALIDITY").unwrap_or(0) as i64;
    let uid_next = status(&responses, "UIDNEXT").unwrap_or(1) as i64;

    let known = store.fetch_folder(account.id, folder.to_owned()).await?;
    let mut state = match known {
        // The provider renumbered the folder: its UIDs say nothing about ours any more, so the
        // count starts again and the remembered message names keep the old ones from coming back.
        Some(known) if known.uid_validity != uid_validity => {
            tracing::info!(address = %account.address, folder, "the provider renumbered this folder");
            FetchFolder { uid_validity, last_uid: 0, held_uid: None, held_since: None }
        }
        Some(known) => known,
        // The first time this folder is seen, only where it stands is written down.
        None => {
            let state = FetchFolder { uid_validity, last_uid: (uid_next - 1).max(0), held_uid: None, held_since: None };
            store.set_fetch_folder(account.id, folder.to_owned(), state).await?;
            tracing::info!(address = %account.address, folder, from = state.last_uid, "fetching starts here");
            return Ok(0);
        }
    };
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
        store.set_fetch_folder(account.id, folder.to_owned(), state).await?;
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
        for message in messages {
            let Some(raw) = message.body else { continue };
            let uid = i64::from(message.uid);
            match take_message(store, smtp, account, to, from_junk, raw).await? {
                Taken::Kept | Taken::Refused(_) => {}
                Taken::Later(answer) => {
                    tracing::info!(address = %account.address, folder, uid, %answer, "left for the next run");
                    held = Some(uid);
                    break 'batches;
                }
            }
            // Only now, with the message really here, is anything changed at the provider.
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
            taken += 1;
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
    store.set_fetch_folder(account.id, folder.to_owned(), state).await?;
    if taken > 0 {
        tracing::info!(address = %account.address, folder, taken, "fetched mail");
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
    if store.mark_fetch_seen(account.id, message_key(&raw)).await? {
        tracing::debug!(address = %account.address, "this message was already here");
        return Ok(Taken::Kept);
    }
    Ok(uwumail_smtp::deliver_fetched(smtp, mailbox_of(account), from_junk, to.to_owned(), raw).await)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uwumail_smtp::{DeliveryConfig, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig};
    use uwumail_store::{IngestRequest, MailboxTarget, NewAccount, NewFetchAccount, Role};

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
        store
            .ingest(IngestRequest {
                account_id: account,
                raw: raw.into_bytes(),
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
                run_once(&store, &smtp, account, Some(detour)).await.unwrap()
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
