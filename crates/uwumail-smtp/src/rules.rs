//! Carrying out what a person's active Sieve script decided for an incoming message
//! (docs/sieve.md): the folders it goes to, its keywords, and a redirect through the same path
//! forwarding takes.
//!
//! A script never gets to do more than the person could do by hand. It files into their own folders
//! only, and it redirects only where forwarding would go: to people on this server, or to an address
//! elsewhere whose owner confirmed it as a forwarding target. Everything else, and every failure,
//! ends with the message in the inbox.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use uwumail_store::{ImapMailbox, IngestRequest, MailboxRole, MailboxTarget, StoreError, normalize_address};

use crate::sieve::{self, Envelope, Folder, Plan, Target};
use crate::{Context, forward};

/// How long a run may take before the message is simply kept. The engine's own limits stop a script
/// long before this; this is for a machine that is very busy.
const RUN_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a message went after the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Filed {
    /// Stored in at least one folder.
    pub stored: bool,
    /// For the history: `inbox` when a copy is in the inbox, `rules` when only in folders the
    /// script chose, nothing when it was discarded or only redirected.
    pub mailbox: Option<&'static str>,
}

/// The account's folders with their paths, the inbox first.
fn folders(mailboxes: &[ImapMailbox]) -> Vec<Folder> {
    let by_id: HashMap<i64, &ImapMailbox> = mailboxes.iter().map(|mailbox| (mailbox.id, mailbox)).collect();
    let path_of = |mailbox: &ImapMailbox| {
        let mut names = Vec::new();
        let mut current = Some(mailbox);
        while let Some(mailbox) = current {
            names.push(mailbox.name.as_str());
            current = mailbox.parent_id.and_then(|parent| by_id.get(&parent).copied());
            if names.len() > 64 {
                break;
            }
        }
        names.reverse();
        names.join("/")
    };
    let mut folders: Vec<(bool, Folder)> = mailboxes
        .iter()
        .map(|mailbox| {
            let inbox = mailbox.role == Some(MailboxRole::Inbox) && mailbox.parent_id.is_none();
            (inbox, Folder { id: mailbox.id, path: path_of(mailbox) })
        })
        .collect();
    folders.sort_by_key(|(inbox, _)| !inbox);
    folders.into_iter().map(|(_, folder)| folder).collect()
}

/// Where script runs take turns. A thread running a script cannot be stopped from outside, and the
/// engine counts instructions, not the work one test does (a `:matches` over a long header is one
/// instruction). So a run that outlives [`RUN_TIMEOUT`] keeps its slot until it is really done, and
/// an account whose run is still going on skips its script meanwhile: runaway scripts can hold at
/// most `slots` threads, never the whole blocking pool that the store needs too.
struct Runs {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    /// Accounts with a run that outlived its timeout, and whether it has finished since.
    overrun: std::sync::Mutex<HashMap<i64, std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    timeout: Duration,
}

/// Why a run gave no plan.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Skipped {
    /// The account's previous run is still going on after its timeout.
    StillRunning,
    /// No slot came free in time.
    Busy,
    TimedOut,
    Crashed(String),
}

impl Runs {
    fn new(slots: usize, timeout: Duration) -> Runs {
        Runs {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(slots)),
            overrun: std::sync::Mutex::new(HashMap::new()),
            timeout,
        }
    }

    fn shared() -> &'static Runs {
        static RUNS: std::sync::OnceLock<Runs> = std::sync::OnceLock::new();
        RUNS.get_or_init(|| {
            let cores = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
            Runs::new((cores / 2).clamp(2, 8), RUN_TIMEOUT)
        })
    }

    /// Runs `work` for `account_id` on the blocking pool, within the limits above.
    async fn run<T: Send + 'static>(
        &self,
        account_id: i64,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Skipped> {
        {
            let mut overrun = self.overrun.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match overrun.get(&account_id) {
                Some(done) if !done.load(std::sync::atomic::Ordering::Acquire) => return Err(Skipped::StillRunning),
                Some(_) => {
                    overrun.remove(&account_id);
                }
                None => {}
            }
        }
        let started = tokio::time::Instant::now();
        let permit = match tokio::time::timeout(self.timeout, self.slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            _ => return Err(Skipped::Busy),
        };
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = done.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = work();
            finished.store(true, std::sync::atomic::Ordering::Release);
            drop(permit);
            result
        });
        match tokio::time::timeout_at(started + self.timeout, task).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(Skipped::Crashed(err.to_string())),
            Err(_) => {
                self.overrun.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(account_id, done);
                Err(Skipped::TimedOut)
            }
        }
    }
}

/// Runs the script on the blocking pool. Any failure is the same as no script: keep.
async fn plan(script: String, message: &[u8], envelope: Envelope<'_>, folders: &[Folder], account_id: i64) -> Plan {
    let (message, from, to, folders) =
        (message.to_vec(), envelope.from.to_owned(), envelope.to.to_owned(), folders.to_vec());
    let run = Runs::shared()
        .run(account_id, move || sieve::run(script.as_bytes(), &message, Envelope { from: &from, to: &to }, &folders))
        .await;
    match run {
        Ok(Ok(plan)) => plan,
        Ok(Err(reason)) => {
            tracing::warn!(account = account_id, %reason, "the sieve script failed, keeping the message in the inbox");
            Plan::keep()
        }
        Err(Skipped::Crashed(err)) => {
            tracing::error!(account = account_id, %err, "the sieve script crashed, keeping the message in the inbox");
            Plan::keep()
        }
        Err(skipped) => {
            let reason = match skipped {
                Skipped::StillRunning => "its last run is still going on",
                Skipped::Busy => "no slot came free in time",
                _ => "it took too long",
            };
            tracing::warn!(
                account = account_id,
                reason,
                "the sieve script did not run, keeping the message in the inbox"
            );
            Plan::keep()
        }
    }
}

/// Finds or makes the folder a filing names. `None` means the inbox.
async fn folder_for(ctx: &Context, account_id: i64, folders: &mut Vec<Folder>, target: &Target) -> Option<i64> {
    let Target::Folder { name, mailbox_id, create } = target else {
        return None;
    };
    // RFC 9042: an id that no longer exists falls back to the name.
    if let Some(number) = mailbox_id.as_deref().and_then(sieve::mailbox_number)
        && folders.iter().any(|folder| folder.id == number)
    {
        return Some(number);
    }
    let inbox = folders.first().map(|folder| folder.path.clone()).unwrap_or_default();
    if let Some(folder) = sieve::find_folder(folders, &inbox, name) {
        return Some(folder.id);
    }
    if !create {
        tracing::info!(
            account = account_id,
            "a sieve script files into a folder that does not exist, keeping it in the inbox"
        );
        return None;
    }
    // RFC 5490 :create -- every missing level along the path.
    let path = sieve::normalize_path(name, &inbox);
    let mut parent: Option<i64> = None;
    let mut current = String::new();
    for level in path.split('/') {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(level);
        if let Some(existing) = folders.iter().find(|folder| folder.path == current) {
            parent = Some(existing.id);
            continue;
        }
        match ctx.store.create_mailbox(account_id, level, parent, None, 0, true).await {
            Ok(id) => {
                folders.push(Folder { id, path: current.clone() });
                parent = Some(id);
            }
            Err(err) => {
                tracing::info!(account = account_id, %err, "a sieve script could not create its folder, keeping it in the inbox");
                return None;
            }
        }
    }
    parent
}

/// Whether a script may redirect to `address`, and where it goes: the account on this server, or
/// `None` for an address elsewhere that is a confirmed forwarding target.
async fn redirect_target(ctx: &Context, account_id: i64, address: &str) -> Option<(String, Option<i64>)> {
    let (local_part, domain) = normalize_address(address).ok()?;
    let address = format!("{local_part}@{domain}");
    match ctx.store.resolve_recipient(&address).await {
        Ok(Some(target)) if target == account_id => None,
        Ok(Some(target)) => Some((address, Some(target))),
        Ok(None) => {
            if !ctx.live().smtp.allow_external_forwarding {
                return None;
            }
            let active = ctx.store.active_forwarding(account_id).await.ok()?;
            let confirmed =
                active.targets.iter().any(|(target, local)| local.is_none() && target.eq_ignore_ascii_case(&address));
            confirmed.then_some((address, None))
        }
        Err(err) => {
            tracing::warn!(account = account_id, %err, "checking a sieve redirect failed");
            None
        }
    }
}

/// Files `message` for one person the way their active script says. `recipient` is the address it
/// arrived for. Errors are from storing it and mean the same as without a script.
pub(crate) async fn deliver(
    ctx: &Context,
    account_id: i64,
    script: String,
    recipient: &str,
    envelope_from: &str,
    message: &[u8],
) -> Result<Filed, StoreError> {
    let mut folders = folders(&ctx.store.imap_mailboxes(account_id).await?);
    let envelope = Envelope { from: envelope_from, to: recipient };
    let plan = plan(script, message, envelope, &folders, account_id).await;

    // Redirects first: one that is not allowed must not lose the message.
    let mut redirected = false;
    let mut refused = false;
    for address in &plan.redirects {
        match redirect_target(ctx, account_id, address).await {
            Some(target) => {
                let login = ctx.store.account_by_id(account_id).await.ok().flatten().map(|account| account.login);
                let name = login.as_deref().unwrap_or(recipient);
                let forwarder = forward::Forwarder { name, account_id: Some(account_id) };
                forward::send(ctx, forwarder, recipient, envelope_from, message, &[target]).await;
                redirected = true;
            }
            None => {
                tracing::info!(
                    account = account_id,
                    "a sieve script redirects where forwarding may not go, not sending it"
                );
                refused = true;
            }
        }
    }

    let mut filings = plan.filings;
    if filings.is_empty() && (refused || !(plan.discarded || redirected)) {
        filings = Plan::keep().filings;
    }

    // One stored message per set of keywords, in every folder that set goes to.
    let inbox_id = folders.first().map(|folder| folder.id);
    let mut groups: BTreeMap<Vec<String>, Vec<i64>> = BTreeMap::new();
    let mut placed: Vec<i64> = Vec::new();
    let mut in_inbox = false;
    for filing in &filings {
        let mailbox = match folder_for(ctx, account_id, &mut folders, &filing.target).await.or(inbox_id) {
            Some(id) => id,
            None => continue,
        };
        in_inbox |= Some(mailbox) == inbox_id;
        if placed.contains(&mailbox) {
            continue;
        }
        placed.push(mailbox);
        groups.entry(filing.keywords.clone()).or_default().push(mailbox);
    }
    if groups.is_empty() && !filings.is_empty() {
        // An account without folders at all: let the store find its inbox.
        groups.insert(Vec::new(), Vec::new());
    }

    let mut stored = false;
    for (keywords, mailboxes) in groups {
        let mailboxes = if mailboxes.is_empty() {
            vec![MailboxTarget::Role(MailboxRole::Inbox)]
        } else {
            mailboxes.into_iter().map(MailboxTarget::Id).collect()
        };
        let request = IngestRequest { account_id, raw: message.to_vec(), mailboxes, keywords, received_at: None };
        match ctx.store.ingest(request).await {
            Ok(_) => stored = true,
            // The first copy decides what the sender hears; a later one failing is only logged.
            Err(err) if !stored => return Err(err),
            Err(err) => {
                tracing::warn!(account = account_id, %err, "storing a further copy of a filtered message failed")
            }
        }
    }
    let mailbox = match (stored, in_inbox) {
        (false, _) => None,
        (true, true) => Some("inbox"),
        (true, false) => Some("rules"),
    };
    Ok(Filed { stored, mailbox })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(id: i64, parent_id: Option<i64>, name: &str, role: Option<MailboxRole>) -> ImapMailbox {
        ImapMailbox { id, parent_id, name: name.into(), role, subscribed: true, uid_validity: 1, uid_next: 1 }
    }

    /// security-audit-0.7.0 S-43: a run that outlives its timeout used to keep its thread busy while
    /// every further message started another one.
    #[tokio::test(flavor = "multi_thread")]
    async fn runaway_runs_hold_their_slot_and_their_account_waits() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runs = Runs::new(1, Duration::from_millis(100));
        let started = std::sync::Arc::new(AtomicUsize::new(0));
        let counting = |sleep_ms: u64| {
            let started = started.clone();
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(sleep_ms));
                "done"
            }
        };

        assert_eq!(runs.run(1, counting(600)).await, Err(Skipped::TimedOut));
        // The same account skips its script while the run goes on, without starting another.
        assert_eq!(runs.run(1, counting(0)).await, Err(Skipped::StillRunning));
        // Everyone else waits for the slot the runaway still holds, and gives up in time.
        assert_eq!(runs.run(2, counting(0)).await, Err(Skipped::Busy));
        assert_eq!(started.load(Ordering::SeqCst), 1, "only the runaway ever ran");

        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(runs.run(1, counting(0)).await, Ok("done"), "once it is done, the account runs again");
        assert_eq!(runs.run(2, counting(0)).await, Ok("done"));
    }

    #[test]
    fn folders_have_paths_and_the_inbox_comes_first() {
        let folders = folders(&[
            mailbox(3, None, "Work", None),
            mailbox(4, Some(3), "Boss", None),
            mailbox(1, None, "Posteingang", Some(MailboxRole::Inbox)),
            mailbox(5, Some(1), "Lists", None),
        ]);
        let paths: Vec<(i64, &str)> = folders.iter().map(|folder| (folder.id, folder.path.as_str())).collect();
        assert_eq!(paths, [(1, "Posteingang"), (3, "Work"), (4, "Work/Boss"), (5, "Posteingang/Lists")]);
        assert_eq!(sieve::find_folder(&folders, "Posteingang", "INBOX/Lists").map(|f| f.id), Some(5));
    }
}
