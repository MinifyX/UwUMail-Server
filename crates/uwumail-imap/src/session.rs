//! One IMAP connection: reading commands, answering them, and telling the client about changes
//! other connections and deliveries made.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::broadcast;
use uwumail_store::{
    Account, AppScope, DELETED_KEYWORD, FlagChange, ImapEmail, IngestRequest, MailAuth, MailAuthDenied, MailboxTarget,
    Store, StoreError,
};

use crate::command::*;
use crate::mailboxes::{self, Named, SEPARATOR};
use crate::parser::{self, ParseError};
use crate::response::{self, Out};
use crate::search;
use crate::{Imap, mime};

const MAX_LINE: usize = 64 * 1024;
/// Everything but APPEND: login data, mailbox names, search words.
const MAX_COMMAND: usize = 256 * 1024;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
/// RFC 3501 asks for at least 30 minutes before logging out an idle client.
const IDLE_CLIENT_TIMEOUT: Duration = Duration::from_secs(31 * 60);
const IDLE_LIMIT: Duration = Duration::from_secs(29 * 60);
const MAX_AUTH_FAILURES: u32 = 3;

pub const CAPABILITIES_BEFORE_LOGIN: &str = "IMAP4rev1 SASL-IR LITERAL+ ID ENABLE IDLE AUTH=PLAIN";

pub fn capabilities_after_login(max_append: usize) -> String {
    format!(
        "IMAP4rev1 LITERAL+ ID ENABLE IDLE NAMESPACE UIDPLUS MOVE UNSELECT CHILDREN SPECIAL-USE LIST-EXTENDED \
         LIST-STATUS ESEARCH CONDSTORE QRESYNC QUOTA QUOTA=RES-STORAGE UTF8=ACCEPT STATUS=SIZE WITHIN \
         APPENDLIMIT={max_append}"
    )
}

/// What a selected mailbox looks like to the client: message sequence numbers are positions in
/// `messages`.
struct Selected {
    mailbox_id: i64,
    read_only: bool,
    messages: Vec<Known>,
    highest_modseq: u64,
}

#[derive(Clone)]
struct Known {
    uid: u32,
    modseq: u64,
    keywords: Vec<String>,
    /// Gone from the mailbox, but the client was not told yet.
    expunged: bool,
}

impl Selected {
    fn largest_uid(&self) -> u32 {
        self.messages.last().map_or(0, |known| known.uid)
    }

    /// Positions (0-based) of the messages a set names, in order.
    fn resolve(&self, set: &SequenceSet, uid: bool) -> Vec<usize> {
        if uid {
            let largest = self.largest_uid();
            self.messages
                .iter()
                .enumerate()
                .filter(|(_, known)| set.contains(known.uid, largest))
                .map(|(index, _)| index)
                .collect()
        } else {
            let count = self.messages.len() as u32;
            (0..self.messages.len()).filter(|index| set.contains(*index as u32 + 1, count)).collect()
        }
    }

    fn position(&self, uid: u32) -> Option<usize> {
        self.messages.binary_search_by_key(&uid, |known| known.uid).ok()
    }
}

enum Flow {
    Continue,
    Logout,
}

enum IdleEvent {
    Line(io::Result<usize>),
    Change,
    Timeout,
}

pub struct Session<R, W> {
    imap: Imap,
    store: Store,
    peer: SocketAddr,
    reader: BufReader<R>,
    writer: W,
    account: Option<Account>,
    auth_failures: u32,
    utf8: bool,
    /// The client uses modseqs: untagged FETCH answers carry MODSEQ.
    condstore: bool,
    qresync: bool,
    selected: Option<Selected>,
    changes: Option<broadcast::Receiver<uwumail_store::StateChange>>,
}

fn no(tag: &str, code: Option<&str>, text: &str) -> String {
    match code {
        Some(code) => format!("{tag} NO [{code}] {text}\r\n"),
        None => format!("{tag} NO {text}\r\n"),
    }
}

fn store_error(tag: &str, err: &StoreError) -> String {
    match err {
        StoreError::NotFound(_) => no(tag, Some("NONEXISTENT"), "No such mailbox"),
        StoreError::QuotaExceeded => no(tag, Some("OVERQUOTA"), "The mailbox is full"),
        StoreError::Rule { message, .. } => no(tag, Some("CANNOT"), message),
        other => {
            tracing::error!(%other, "imap command failed in the store");
            no(tag, Some("SERVERBUG"), "Something went wrong, please try again")
        }
    }
}

impl<R, W> Session<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(imap: Imap, peer: SocketAddr, reader: R, writer: W) -> Self {
        // Canonicalise an IPv4-mapped IPv6 peer to plain IPv4, like the SMTP and HTTP listeners do.
        // Otherwise the login limiter's /64 key folds every `::ffff:a.b.c.d` to `::` -- one IPv4
        // client would throttle all of them -- and a ban reported to the gateway would name an
        // address fail2ban cannot match (security-audit-0.5.2 S-27).
        let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
        Session {
            store: imap.store.clone(),
            imap,
            peer,
            reader: BufReader::new(reader),
            writer,
            account: None,
            auth_failures: 0,
            utf8: false,
            condstore: false,
            qresync: false,
            selected: None,
            changes: None,
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        self.writer.flush().await
    }

    pub async fn run(mut self) -> io::Result<()> {
        let greeting = format!("* OK [CAPABILITY {CAPABILITIES_BEFORE_LOGIN}] UwUMail IMAP ready\r\n");
        self.send(greeting.as_bytes()).await?;
        self.flush().await?;
        loop {
            let limit = if self.account.is_some() { IDLE_CLIENT_TIMEOUT } else { LOGIN_TIMEOUT };
            let command = match tokio::time::timeout(limit, self.read_command()).await {
                Ok(Ok(Some(command))) => command,
                Ok(Ok(None)) => return Ok(()),
                Ok(Err(err)) if err.kind() == io::ErrorKind::InvalidData => {
                    self.send(format!("* BYE {err}\r\n").as_bytes()).await?;
                    self.flush().await?;
                    return Ok(());
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    self.send(b"* BYE Autologout, you were idle for too long\r\n").await?;
                    self.flush().await?;
                    return Ok(());
                }
            };
            let flow = match parser::parse_command(&command, self.utf8) {
                Ok(command) => self.dispatch(command).await?,
                Err(ParseError { tag, message }) => {
                    let tag = tag.unwrap_or_else(|| "*".into());
                    if let Some(code) = message.strip_prefix('[').and_then(|m| m.split_once(']')) {
                        self.send(format!("{tag} NO [{}]{}\r\n", code.0, code.1).as_bytes()).await?;
                    } else {
                        self.send(format!("{tag} BAD {message}\r\n").as_bytes()).await?;
                    }
                    Flow::Continue
                }
            };
            self.flush().await?;
            if matches!(flow, Flow::Logout) {
                return Ok(());
            }
        }
    }

    async fn read_line(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        let read = (&mut self.reader).take(limit as u64 + 1).read_until(b'\n', &mut line).await?;
        if read > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Line too long"));
        }
        Ok(line)
    }

    /// Reads a command with its literals. `None` when the client hung up.
    async fn read_command(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut command = Vec::new();
        loop {
            let line = self.read_line(MAX_LINE).await?;
            if line.is_empty() {
                return Ok(None);
            }
            if !line.ends_with(b"\n") {
                return Ok(None);
            }
            command.extend_from_slice(&line);
            let Some((size, non_synchronizing)) = parser::literal_announcement(&line) else {
                return Ok(Some(command));
            };
            let is_append = String::from_utf8_lossy(&command[..command.len().min(64)])
                .split(' ')
                .nth(1)
                .is_some_and(|word| word.eq_ignore_ascii_case("APPEND"));
            // The generous append limit is for people who are logged in. Before that, a stranger
            // could announce a literal of the full message size and make the server set aside that
            // much -- the bytes are never sent, the memory is held until the login times out, and
            // one short line per connection is all it costs. Nothing before a login needs more than
            // a command.
            let limit = if is_append && self.account.is_some() { self.imap.max_append } else { MAX_COMMAND };
            if size > limit || command.len() + size > limit + MAX_COMMAND {
                if non_synchronizing {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Literal too big"));
                }
                let tag = String::from_utf8_lossy(&command).split(' ').next().unwrap_or("*").to_owned();
                let answer = if is_append {
                    no(&tag, Some("TOOBIG"), "The message is too big")
                } else {
                    format!("{tag} BAD Too much data\r\n")
                };
                self.send(answer.as_bytes()).await?;
                self.flush().await?;
                command.clear();
                continue;
            }
            if !non_synchronizing {
                self.send(b"+ Ready for literal data\r\n").await?;
                self.flush().await?;
            }
            let start = command.len();
            command.resize(start + size, 0);
            self.reader.read_exact(&mut command[start..]).await?;
        }
    }

    async fn dispatch(&mut self, command: Command) -> io::Result<Flow> {
        let tag = command.tag.clone();
        let authenticated = self.account.is_some();
        let needs_selection = matches!(
            command.body,
            CommandBody::Close
                | CommandBody::Unselect
                | CommandBody::Expunge { .. }
                | CommandBody::Search { .. }
                | CommandBody::Fetch { .. }
                | CommandBody::Store { .. }
                | CommandBody::Copy { .. }
                | CommandBody::Move { .. }
                | CommandBody::Check
        );
        match &command.body {
            CommandBody::Capability | CommandBody::Noop | CommandBody::Logout | CommandBody::Id => {}
            CommandBody::Login { .. } | CommandBody::Authenticate { .. } | CommandBody::StartTls if authenticated => {
                self.send(format!("{tag} BAD Already logged in\r\n").as_bytes()).await?;
                return Ok(Flow::Continue);
            }
            CommandBody::Login { .. } | CommandBody::Authenticate { .. } | CommandBody::StartTls => {}
            CommandBody::Enable(_) if !authenticated => {
                self.send(format!("{tag} BAD Log in first\r\n").as_bytes()).await?;
                return Ok(Flow::Continue);
            }
            _ if !authenticated => {
                self.send(format!("{tag} BAD Log in first\r\n").as_bytes()).await?;
                return Ok(Flow::Continue);
            }
            _ if needs_selection && self.selected.is_none() => {
                self.send(format!("{tag} BAD Select a mailbox first\r\n").as_bytes()).await?;
                return Ok(Flow::Continue);
            }
            _ => {}
        }

        let result = match command.body {
            CommandBody::Capability => {
                let capabilities = match self.account {
                    Some(_) => capabilities_after_login(self.imap.max_append),
                    None => CAPABILITIES_BEFORE_LOGIN.to_owned(),
                };
                self.send(format!("* CAPABILITY {capabilities}\r\n").as_bytes()).await?;
                Ok(format!("{tag} OK Capability completed\r\n"))
            }
            CommandBody::Noop | CommandBody::Check => {
                self.refresh(true).await?;
                Ok(format!("{tag} OK Nothing to do, all fine\r\n"))
            }
            CommandBody::Logout => {
                self.send(b"* BYE See you soon\r\n").await?;
                self.send(format!("{tag} OK Logout completed\r\n").as_bytes()).await?;
                return Ok(Flow::Logout);
            }
            CommandBody::StartTls => Ok(format!("{tag} BAD The connection is encrypted already\r\n")),
            CommandBody::Id => {
                self.send(b"* ID (\"name\" \"UwUMail\" \"vendor\" \"UwUMail\")\r\n").await?;
                Ok(format!("{tag} OK Id completed\r\n"))
            }
            CommandBody::Login { username, password } => return self.login(&tag, &username, &password).await,
            CommandBody::Authenticate { mechanism, initial } => {
                return self.authenticate(&tag, &mechanism, initial).await;
            }
            CommandBody::Enable(capabilities) => {
                let mut enabled = Vec::new();
                for capability in capabilities {
                    match capability.as_str() {
                        "CONDSTORE" => self.condstore = true,
                        "QRESYNC" => {
                            self.qresync = true;
                            self.condstore = true;
                        }
                        "UTF8=ACCEPT" => self.utf8 = true,
                        _ => continue,
                    }
                    enabled.push(capability);
                }
                let names: String = enabled.iter().map(|name| format!(" {name}")).collect();
                self.send(format!("* ENABLED{names}\r\n").as_bytes()).await?;
                Ok(format!("{tag} OK Enabled\r\n"))
            }
            CommandBody::Namespace => {
                self.send(format!("* NAMESPACE ((\"\" \"{SEPARATOR}\")) NIL NIL\r\n").as_bytes()).await?;
                Ok(format!("{tag} OK Namespace completed\r\n"))
            }
            CommandBody::Select { mailbox, read_only, condstore, qresync } => {
                self.select(&tag, &mailbox, read_only, condstore, qresync).await
            }
            CommandBody::Create { mailbox } => self.create(&tag, &mailbox).await,
            CommandBody::Delete { mailbox } => self.delete(&tag, &mailbox).await,
            CommandBody::Rename { from, to } => self.rename(&tag, &from, &to).await,
            CommandBody::Subscribe { mailbox } => self.subscribe(&tag, &mailbox, true).await,
            CommandBody::Unsubscribe { mailbox } => self.subscribe(&tag, &mailbox, false).await,
            CommandBody::List(list) => self.list(&tag, list, "LIST").await,
            CommandBody::Lsub { reference, pattern } => {
                let list = ListCommand {
                    reference,
                    patterns: vec![pattern],
                    subscribed: true,
                    extended: false,
                    ..Default::default()
                };
                self.list(&tag, list, "LSUB").await
            }
            CommandBody::Status { mailbox, items } => self.status(&tag, &mailbox, &items).await,
            CommandBody::Append { mailbox, flags, date, message } => {
                self.append(&tag, &mailbox, flags, date, message).await
            }
            CommandBody::Idle => return self.idle(&tag).await,
            CommandBody::Close | CommandBody::Unselect => {
                let close = matches!(command.body, CommandBody::Close);
                if let Some(selected) = self.selected.take()
                    && close
                    && !selected.read_only
                {
                    let account = self.account_id();
                    if let Err(err) = self.store.imap_expunge(account, selected.mailbox_id, None).await {
                        tracing::warn!(%err, "expunging on CLOSE failed");
                    }
                }
                if self.qresync {
                    self.send(b"* OK [CLOSED] Previous mailbox closed\r\n").await?;
                }
                Ok(format!("{tag} OK Closed\r\n"))
            }
            CommandBody::Expunge { uids } => self.expunge(&tag, uids).await,
            CommandBody::Search { uid, returns, criteria } => self.search(&tag, uid, returns, criteria).await,
            CommandBody::Fetch { uid, set, items, changed_since, vanished } => {
                self.fetch(&tag, uid, set, items, changed_since, vanished).await
            }
            CommandBody::Store { uid, set, unchanged_since, action, silent, flags } => {
                self.store_flags(&tag, uid, set, unchanged_since, action, silent, flags).await
            }
            CommandBody::Copy { uid, set, mailbox } => self.copy(&tag, uid, set, &mailbox, false).await,
            CommandBody::Move { uid, set, mailbox } => self.copy(&tag, uid, set, &mailbox, true).await,
            CommandBody::GetQuota { root } => self.quota(&tag, Some(root), None).await,
            CommandBody::GetQuotaRoot { mailbox } => self.quota(&tag, None, Some(mailbox)).await,
        };
        let answer = match result {
            Ok(answer) => answer,
            Err(err) => store_error(&tag, &err),
        };
        self.send(answer.as_bytes()).await?;
        Ok(Flow::Continue)
    }

    fn account_id(&self) -> i64 {
        self.account.as_ref().map_or(0, |account| account.id)
    }

    async fn named(&mut self) -> Result<Vec<Named>, StoreError> {
        Ok(mailboxes::named(self.store.imap_mailboxes(self.account_id()).await?))
    }

    // ---- logging in ----

    async fn login(&mut self, tag: &str, username: &str, password: &str) -> io::Result<Flow> {
        if self.imap.limiter.is_blocked(self.peer.ip()) {
            self.send(no(tag, Some("UNAVAILABLE"), "Too many failed logins, try again later").as_bytes()).await?;
            return Ok(Flow::Continue);
        }
        let peer = self.peer.to_string();
        match self.store.authenticate_mail(username, password, AppScope::Mail, "imap", &peer).await {
            Ok(MailAuth::Ok { account, app_password }) => {
                self.imap.limiter.record_success(self.peer.ip(), username);
                tracing::info!(login = %account.login, peer = %self.peer, app_password = app_password.is_some(), "imap login");
                self.changes = Some(self.store.subscribe_changes());
                self.account = Some(account);
                let capabilities = capabilities_after_login(self.imap.max_append);
                self.send(format!("{tag} OK [CAPABILITY {capabilities}] Logged in, hi\r\n").as_bytes()).await?;
                Ok(Flow::Continue)
            }
            Ok(MailAuth::Denied(reason)) => {
                match reason {
                    // A phone still using the right account password should not lock out its network.
                    MailAuthDenied::AppPasswordRequired => {}
                    MailAuthDenied::UnknownLogin => self.imap.limiter.record_unknown_login(self.peer.ip()),
                    _ => self.imap.limiter.record_failure(self.peer.ip(), username),
                }
                self.auth_failures += 1;
                tracing::warn!(login = %username, peer = %self.peer, %reason, "failed imap login");
                tokio::time::sleep(Duration::from_secs(1)).await;
                let text = match reason {
                    MailAuthDenied::AppPasswordRequired => "This account needs an app password for mail apps",
                    _ => "Wrong login or password",
                };
                self.send(no(tag, Some("AUTHENTICATIONFAILED"), text).as_bytes()).await?;
                if self.auth_failures >= MAX_AUTH_FAILURES {
                    self.send(b"* BYE Too many failed logins\r\n").await?;
                    return Ok(Flow::Logout);
                }
                Ok(Flow::Continue)
            }
            Err(err) => {
                tracing::error!(%err, "imap authentication failed internally");
                self.send(no(tag, Some("UNAVAILABLE"), "Temporary authentication failure").as_bytes()).await?;
                Ok(Flow::Continue)
            }
        }
    }

    async fn authenticate(&mut self, tag: &str, mechanism: &str, initial: Option<String>) -> io::Result<Flow> {
        if mechanism != "PLAIN" {
            self.send(no(tag, Some("CANNOT"), "Only PLAIN is supported").as_bytes()).await?;
            return Ok(Flow::Continue);
        }
        let response = match initial {
            Some(initial) => initial,
            None => {
                self.send(b"+ \r\n").await?;
                self.flush().await?;
                let line = self.read_line(MAX_LINE).await?;
                match parser::parse_continuation(&line) {
                    Some(response) => response.to_owned(),
                    None => {
                        self.send(format!("{tag} BAD Authentication cancelled\r\n").as_bytes()).await?;
                        return Ok(Flow::Continue);
                    }
                }
            }
        };
        let decoded = if response == "=" {
            Some(Vec::new())
        } else {
            base64::engine::general_purpose::STANDARD.decode(response.trim()).ok()
        };
        let parts: Option<Vec<String>> = decoded.and_then(|bytes| {
            let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
            (parts.len() == 3).then(|| parts.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect())
        });
        let Some(parts) = parts else {
            self.send(format!("{tag} BAD Invalid PLAIN data\r\n").as_bytes()).await?;
            return Ok(Flow::Continue);
        };
        if !parts[0].is_empty() && !parts[0].eq_ignore_ascii_case(&parts[1]) {
            self.send(no(tag, Some("AUTHORIZATIONFAILED"), "Logging in as someone else is not allowed").as_bytes())
                .await?;
            return Ok(Flow::Continue);
        }
        let (username, password) = (parts[1].clone(), parts[2].clone());
        self.login(tag, &username, &password).await
    }

    // ---- mailboxes ----

    async fn create(&mut self, tag: &str, path: &str) -> Result<String, StoreError> {
        let path = mailboxes::normalize(path);
        if path.is_empty() || path.split(SEPARATOR).any(|level| level.trim().is_empty()) {
            return Ok(format!("{tag} BAD The mailbox name is empty\r\n"));
        }
        let named = self.named().await?;
        if mailboxes::find(&named, &path).is_some() {
            return Ok(no(tag, Some("ALREADYEXISTS"), "The mailbox exists already"));
        }
        let account = self.account_id();
        let mut parent: Option<i64> = None;
        let mut current = String::new();
        let levels: Vec<&str> = path.split(SEPARATOR).collect();
        for (index, level) in levels.iter().enumerate() {
            if !current.is_empty() {
                current.push(SEPARATOR);
            }
            current.push_str(level);
            if let Some(existing) = mailboxes::find(&named, &current) {
                parent = Some(existing.mailbox.id);
                continue;
            }
            let id = self.store.create_mailbox(account, level, parent, None, 0, true).await?;
            parent = Some(id);
            if index + 1 == levels.len() {
                break;
            }
        }
        Ok(format!("{tag} OK Create completed\r\n"))
    }

    async fn delete(&mut self, tag: &str, path: &str) -> Result<String, StoreError> {
        let named = self.named().await?;
        let Some(found) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
        };
        if found.path == "INBOX" {
            return Ok(no(tag, Some("CANNOT"), "The INBOX cannot be deleted"));
        }
        if found.has_children {
            return Ok(no(tag, Some("INUSE"), "Delete the mailboxes inside it first"));
        }
        let id = found.mailbox.id;
        if self.selected.as_ref().is_some_and(|selected| selected.mailbox_id == id) {
            self.selected = None;
        }
        self.store.destroy_mailbox(self.account_id(), id, true).await?;
        Ok(format!("{tag} OK Delete completed\r\n"))
    }

    async fn rename(&mut self, tag: &str, from: &str, to: &str) -> Result<String, StoreError> {
        let named = self.named().await?;
        let Some(source) = mailboxes::find(&named, from) else {
            return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
        };
        if source.path == "INBOX" {
            return Ok(no(tag, Some("CANNOT"), "The INBOX cannot be renamed"));
        }
        let target = mailboxes::normalize(to);
        if mailboxes::find(&named, &target).is_some() {
            return Ok(no(tag, Some("ALREADYEXISTS"), "A mailbox with that name exists already"));
        }
        let (parent_path, name) = match target.rsplit_once(SEPARATOR) {
            Some((parent, name)) => (Some(parent.to_owned()), name.to_owned()),
            None => (None, target.clone()),
        };
        if name.trim().is_empty() {
            return Ok(format!("{tag} BAD The mailbox name is empty\r\n"));
        }
        let parent_id = match parent_path {
            Some(parent_path) => {
                if mailboxes::find(&named, &parent_path).is_none() {
                    self.create(tag, &parent_path).await?;
                }
                let named = self.named().await?;
                match mailboxes::find(&named, &parent_path) {
                    Some(parent) => Some(parent.mailbox.id),
                    None => return Ok(no(tag, Some("CANNOT"), "The parent mailbox could not be created")),
                }
            }
            None => None,
        };
        let update =
            uwumail_store::MailboxUpdate { name: Some(name), parent_id: Some(parent_id), ..Default::default() };
        self.store.update_mailbox(self.account_id(), source.mailbox.id, update).await?;
        Ok(format!("{tag} OK Rename completed\r\n"))
    }

    async fn subscribe(&mut self, tag: &str, path: &str, subscribed: bool) -> Result<String, StoreError> {
        let named = self.named().await?;
        let Some(found) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
        };
        let update = uwumail_store::MailboxUpdate { subscribed: Some(subscribed), ..Default::default() };
        self.store.update_mailbox(self.account_id(), found.mailbox.id, update).await?;
        Ok(format!("{tag} OK Done\r\n"))
    }

    async fn list(&mut self, tag: &str, list: ListCommand, verb: &str) -> Result<String, StoreError> {
        let command_name = if verb == "LSUB" { "Lsub" } else { "List" };
        if list.patterns.iter().all(|pattern| pattern.is_empty()) {
            self.send(format!("* {verb} (\\Noselect) \"{SEPARATOR}\" \"\"\r\n").as_bytes()).await.ok();
            return Ok(format!("{tag} OK {command_name} completed\r\n"));
        }
        let named = self.named().await?;
        for mailbox in &named {
            let matched = list.patterns.iter().any(|pattern| {
                let full = if list.reference.is_empty() {
                    pattern.clone()
                } else {
                    format!("{}{SEPARATOR}{pattern}", list.reference.trim_end_matches(SEPARATOR))
                };
                mailboxes::matches(&full, &mailbox.path)
            });
            if !matched || (list.subscribed && !mailbox.mailbox.subscribed) {
                continue;
            }
            let special = mailboxes::special_use(mailbox.mailbox.role);
            if list.special_use && special.is_none() {
                continue;
            }
            let mut attributes = Vec::new();
            attributes.push(if mailbox.has_children { "\\HasChildren" } else { "\\HasNoChildren" });
            if mailbox.mailbox.subscribed && (list.subscribed || list.return_subscribed) {
                attributes.push("\\Subscribed");
            }
            if let Some(special) = special {
                attributes.push(special);
            }
            let mut out = Out::new(self.utf8);
            out.raw(&format!("* {verb} ({}) \"{SEPARATOR}\" ", attributes.join(" ")))
                .mailbox(&mailbox.path)
                .raw("\r\n");
            if let Some(items) = &list.return_status {
                let status = self.store.imap_status(self.account_id(), mailbox.mailbox.id).await?;
                out.raw("* STATUS ").mailbox(&mailbox.path).raw(&format!(" ({})\r\n", status_items(items, &status)));
            }
            self.send(&out.bytes).await.map_err(|err| StoreError::Internal(err.to_string()))?;
        }
        Ok(format!("{tag} OK {command_name} completed\r\n"))
    }

    async fn status(&mut self, tag: &str, path: &str, items: &[StatusItem]) -> Result<String, StoreError> {
        let named = self.named().await?;
        let Some(found) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
        };
        let status = self.store.imap_status(self.account_id(), found.mailbox.id).await?;
        let mut out = Out::new(self.utf8);
        out.raw("* STATUS ").mailbox(&found.path).raw(&format!(" ({})\r\n", status_items(items, &status)));
        self.send(&out.bytes).await.map_err(|err| StoreError::Internal(err.to_string()))?;
        Ok(format!("{tag} OK Status completed\r\n"))
    }

    async fn quota(&mut self, tag: &str, root: Option<String>, mailbox: Option<String>) -> Result<String, StoreError> {
        let Some(account) = self.store.account_by_id(self.account_id()).await? else {
            return Ok(no(tag, Some("SERVERBUG"), "The account is gone"));
        };
        if let Some(root) = root
            && !root.is_empty()
        {
            return Ok(no(tag, Some("NONEXISTENT"), "No such quota root"));
        }
        if let Some(mailbox) = mailbox {
            let named = self.named().await?;
            let Some(found) = mailboxes::find(&named, &mailbox) else {
                return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
            };
            let mut out = Out::new(self.utf8);
            out.raw("* QUOTAROOT ").mailbox(&found.path).raw(" \"\"\r\n");
            self.send(&out.bytes).await.map_err(|err| StoreError::Internal(err.to_string()))?;
        }
        let quota = if account.quota_bytes > 0 {
            format!("* QUOTA \"\" (STORAGE {} {})\r\n", account.used_bytes.max(0) / 1024, account.quota_bytes / 1024)
        } else {
            "* QUOTA \"\" ()\r\n".to_owned()
        };
        self.send(quota.as_bytes()).await.map_err(|err| StoreError::Internal(err.to_string()))?;
        Ok(format!("{tag} OK Quota completed\r\n"))
    }

    async fn append(
        &mut self,
        tag: &str,
        path: &str,
        flags: Vec<String>,
        date: Option<i64>,
        message: Vec<u8>,
    ) -> Result<String, StoreError> {
        let named = self.named().await?;
        let Some(found) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("TRYCREATE"), "No such mailbox"));
        };
        if message.is_empty() {
            return Ok(format!("{tag} BAD The message is empty\r\n"));
        }
        let keywords: Vec<String> = flags.iter().filter_map(|flag| parser::keyword_of_flag(flag)).collect();
        let request = IngestRequest {
            account_id: self.account_id(),
            raw: message,
            mailboxes: vec![MailboxTarget::Id(found.mailbox.id)],
            keywords,
            received_at: date,
        };
        let email = self.store.ingest(request).await?;
        let uid_validity = found.mailbox.uid_validity;
        self.refresh(false).await.map_err(|err| StoreError::Internal(err.to_string()))?;
        Ok(format!("{tag} OK [APPENDUID {uid_validity} {}] Append completed\r\n", email.uid))
    }

    async fn select(
        &mut self,
        tag: &str,
        path: &str,
        read_only: bool,
        condstore: bool,
        qresync: Option<QresyncParams>,
    ) -> Result<String, StoreError> {
        if self.selected.take().is_some() && self.qresync {
            self.send(b"* OK [CLOSED] Previous mailbox closed\r\n").await.map_err(io_error)?;
        }
        if condstore || qresync.is_some() {
            self.condstore = true;
        }
        let named = self.named().await?;
        let Some(found) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("NONEXISTENT"), "No such mailbox"));
        };
        let account = self.account_id();
        let state = self.store.imap_messages(account, found.mailbox.id).await?;

        let mut keywords: Vec<String> = state.messages.iter().flat_map(|m| m.keywords.iter().cloned()).collect();
        keywords.sort();
        keywords.dedup();
        let custom: Vec<String> = keywords.into_iter().filter(|k| !is_system_keyword(k)).collect();
        let first_unseen = state.messages.iter().position(|m| !m.keywords.iter().any(|k| k == "$seen"));

        let mut out = Out::new(self.utf8);
        out.raw(&format!("* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft{})\r\n", space_list(&custom)));
        if read_only {
            out.raw("* OK [PERMANENTFLAGS ()] Read-only mailbox\r\n");
        } else {
            out.raw("* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted\r\n");
        }
        out.raw(&format!("* {} EXISTS\r\n* 0 RECENT\r\n", state.messages.len()));
        if let Some(first) = first_unseen {
            out.raw(&format!("* OK [UNSEEN {}] First unseen\r\n", first + 1));
        }
        out.raw(&format!("* OK [UIDVALIDITY {}] UIDs valid\r\n", state.uid_validity));
        out.raw(&format!("* OK [UIDNEXT {}] Predicted next UID\r\n", state.uid_next));
        out.raw(&format!("* OK [HIGHESTMODSEQ {}] Highest\r\n", state.highest_modseq));
        if let Some(qresync) = &qresync
            && qresync.uid_validity == state.uid_validity
        {
            let mut vanished = self.store.imap_vanished(account, found.mailbox.id, qresync.modseq).await?;
            if let Some(known) = &qresync.known_uids {
                let largest = u32::MAX;
                vanished.retain(|uid| known.contains(*uid, largest));
            }
            if !vanished.is_empty() {
                out.raw(&format!("* VANISHED (EARLIER) {}\r\n", response::sequence_set(&vanished)));
            }
            for (index, message) in state.messages.iter().enumerate() {
                if message.modseq > qresync.modseq {
                    out.raw(&format!(
                        "* {} FETCH (UID {} FLAGS {} MODSEQ ({}))\r\n",
                        index + 1,
                        message.uid,
                        response::flags(&message.keywords),
                        message.modseq
                    ));
                }
            }
        }
        self.send(&out.bytes).await.map_err(io_error)?;

        self.selected = Some(Selected {
            mailbox_id: found.mailbox.id,
            read_only,
            highest_modseq: state.highest_modseq,
            messages: state
                .messages
                .into_iter()
                .map(|m| Known { uid: m.uid, modseq: m.modseq, keywords: m.keywords, expunged: false })
                .collect(),
        });
        let mode = if read_only { "READ-ONLY" } else { "READ-WRITE" };
        Ok(format!("{tag} OK [{mode}] Select completed\r\n"))
    }

    // ---- changes ----

    /// Tells the client what changed in the selected mailbox. Expunges are only reported where
    /// the protocol allows them; until then the messages stay as placeholders.
    async fn refresh(&mut self, report_expunges: bool) -> io::Result<()> {
        let account = self.account_id();
        let Some(selected) = self.selected.as_ref() else {
            return Ok(());
        };
        let pending_expunges = selected.messages.iter().any(|known| known.expunged);
        let modseq = self.store.account_modseq(account).await.map_err(io::Error::other)?.max(0) as u64;
        if modseq == selected.highest_modseq && !(report_expunges && pending_expunges) {
            return Ok(());
        }
        let mailbox_id = selected.mailbox_id;
        let state = match self.store.imap_messages(account, mailbox_id).await {
            Ok(state) => state,
            Err(StoreError::NotFound(_)) => {
                self.selected = None;
                self.send(b"* BYE The selected mailbox was deleted\r\n").await?;
                self.flush().await?;
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "mailbox deleted"));
            }
            Err(err) => return Err(io::Error::other(err)),
        };
        let (utf8, condstore, qresync) = (self.utf8, self.condstore, self.qresync);
        let selected = self.selected.as_mut().expect("checked above");
        let current: HashMap<u32, &uwumail_store::ImapMessage> = state.messages.iter().map(|m| (m.uid, m)).collect();
        let mut out = Out::new(utf8);

        for known in selected.messages.iter_mut() {
            if !current.contains_key(&known.uid) {
                known.expunged = true;
            }
        }
        if report_expunges {
            let gone: Vec<u32> =
                selected.messages.iter().filter(|known| known.expunged).map(|known| known.uid).collect();
            if !gone.is_empty() {
                if qresync {
                    out.raw(&format!("* VANISHED {}\r\n", response::sequence_set(&gone)));
                    selected.messages.retain(|known| !known.expunged);
                } else {
                    for index in (0..selected.messages.len()).rev() {
                        if selected.messages[index].expunged {
                            out.raw(&format!("* {} EXPUNGE\r\n", index + 1));
                            selected.messages.remove(index);
                        }
                    }
                }
            }
        }
        for (index, known) in selected.messages.iter_mut().enumerate() {
            let Some(message) = current.get(&known.uid) else { continue };
            if message.keywords != known.keywords || message.modseq != known.modseq {
                known.keywords = message.keywords.clone();
                known.modseq = message.modseq;
                out.raw(&format!("* {} FETCH (FLAGS {}", index + 1, response::flags(&known.keywords)));
                if condstore {
                    out.raw(&format!(" UID {} MODSEQ ({})", known.uid, known.modseq));
                }
                out.raw(")\r\n");
            }
        }
        let largest = selected.largest_uid();
        let new: Vec<Known> = state
            .messages
            .iter()
            .filter(|m| m.uid > largest)
            .map(|m| Known { uid: m.uid, modseq: m.modseq, keywords: m.keywords.clone(), expunged: false })
            .collect();
        if !new.is_empty() {
            selected.messages.extend(new);
            out.raw(&format!("* {} EXISTS\r\n* 0 RECENT\r\n", selected.messages.len()));
        }
        selected.highest_modseq = state.highest_modseq;
        self.send(&out.bytes).await
    }

    async fn idle(&mut self, tag: &str) -> io::Result<Flow> {
        self.refresh(true).await?;
        self.send(b"+ idling\r\n").await?;
        self.flush().await?;
        let account = self.account_id();
        let deadline = tokio::time::Instant::now() + IDLE_LIMIT;
        let mut changes = self.changes.take().unwrap_or_else(|| self.store.subscribe_changes());
        // Kept across wakeups: a change can interrupt a half-read line.
        let mut line = Vec::new();
        let result = loop {
            let event = tokio::select! {
                read = read_idle_line(&mut self.reader, &mut line) => IdleEvent::Line(read),
                change = changes.recv() => match change {
                    Ok(change) if change.account_id != account => continue,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => IdleEvent::Change,
                    Err(broadcast::error::RecvError::Closed) => IdleEvent::Timeout,
                },
                _ = tokio::time::sleep_until(deadline) => IdleEvent::Timeout,
            };
            match event {
                IdleEvent::Change => {
                    self.refresh(true).await?;
                    self.flush().await?;
                }
                IdleEvent::Timeout => {
                    self.send(b"* BYE Idled for too long, please reconnect\r\n").await?;
                    break Ok(Flow::Logout);
                }
                IdleEvent::Line(Ok(0)) => break Ok(Flow::Logout),
                IdleEvent::Line(Ok(_)) if !line.ends_with(b"\n") => continue,
                IdleEvent::Line(Ok(_)) => {
                    if String::from_utf8_lossy(&line).trim().eq_ignore_ascii_case("DONE") {
                        self.send(format!("{tag} OK Idle completed\r\n").as_bytes()).await?;
                    } else {
                        self.send(format!("{tag} BAD Expected DONE\r\n").as_bytes()).await?;
                    }
                    break Ok(Flow::Continue);
                }
                IdleEvent::Line(Err(err)) => break Err(err),
            }
        };
        self.changes = Some(changes);
        result
    }

    // ---- messages ----

    fn selected(&self) -> &Selected {
        self.selected.as_ref().expect("the command needs a selected mailbox")
    }

    async fn expunge(&mut self, tag: &str, uids: Option<SequenceSet>) -> Result<String, StoreError> {
        let selected = self.selected();
        if selected.read_only {
            return Ok(no(tag, Some("CANNOT"), "The mailbox is read-only"));
        }
        let (mailbox_id, largest) = (selected.mailbox_id, selected.largest_uid());
        let uid_list = uids.map(|set| {
            selected.messages.iter().map(|known| known.uid).filter(|uid| set.contains(*uid, largest)).collect()
        });
        let removed = self.store.imap_expunge(self.account_id(), mailbox_id, uid_list).await?;
        self.report_removed(&removed).await.map_err(io_error)?;
        self.refresh(true).await.map_err(io_error)?;
        Ok(format!("{tag} OK Expunge completed\r\n"))
    }

    /// Reports messages this session removed, right away.
    async fn report_removed(&mut self, uids: &[u32]) -> io::Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        let qresync = self.qresync;
        let selected = self.selected.as_mut().expect("selected");
        let mut out = Out::new(false);
        if qresync {
            out.raw(&format!("* VANISHED {}\r\n", response::sequence_set(uids)));
            selected.messages.retain(|known| !uids.contains(&known.uid));
        } else {
            let mut positions: Vec<usize> = uids.iter().filter_map(|uid| selected.position(*uid)).collect();
            positions.sort_unstable();
            for index in positions.into_iter().rev() {
                out.raw(&format!("* {} EXPUNGE\r\n", index + 1));
                selected.messages.remove(index);
            }
        }
        self.send(&out.bytes).await
    }

    async fn search(
        &mut self,
        tag: &str,
        uid: bool,
        returns: Option<Vec<SearchReturn>>,
        criteria: SearchKey,
    ) -> Result<String, StoreError> {
        let account = self.account_id();
        let selected = self.selected();
        let mailbox_id = selected.mailbox_id;
        let uids: Vec<u32> = selected.messages.iter().filter(|known| !known.expunged).map(|known| known.uid).collect();
        let positions: HashMap<u32, u32> =
            selected.messages.iter().enumerate().map(|(index, known)| (known.uid, index as u32 + 1)).collect();
        let scope = search::Scope {
            largest_msn: selected.messages.len() as u32,
            largest_uid: selected.largest_uid(),
            now: now(),
        };
        let emails = self.store.imap_emails(account, mailbox_id, uids).await?;
        let prepared = search::prepare(&self.store, account, &criteria, &emails).await?;
        let mut found = Vec::new();
        let mut highest = 0;
        for email in &emails {
            let msn = positions[&email.uid];
            if search::matches(&criteria, &search::Target { msn, email }, &scope, &prepared) {
                found.push(if uid { email.uid } else { msn });
                highest = highest.max(email.modseq);
            }
        }
        found.sort_unstable();
        let with_modseq = search::uses_modseq(&criteria);
        if with_modseq {
            self.condstore = true;
        }
        let mut out = Out::new(self.utf8);
        match returns {
            None => {
                out.raw("* SEARCH");
                for number in &found {
                    out.raw(&format!(" {number}"));
                }
                if with_modseq && !found.is_empty() {
                    out.raw(&format!(" (MODSEQ {highest})"));
                }
                out.raw("\r\n");
            }
            Some(options) => {
                out.raw("* ESEARCH (TAG ").string(tag.as_bytes()).raw(")");
                if uid {
                    out.raw(" UID");
                }
                if !found.is_empty() {
                    for option in &options {
                        match option {
                            SearchReturn::Min => out.raw(&format!(" MIN {}", found[0])),
                            SearchReturn::Max => out.raw(&format!(" MAX {}", found[found.len() - 1])),
                            SearchReturn::All => out.raw(&format!(" ALL {}", response::sequence_set(&found))),
                            SearchReturn::Count => out.raw(&format!(" COUNT {}", found.len())),
                        };
                    }
                    if with_modseq {
                        out.raw(&format!(" MODSEQ {highest}"));
                    }
                } else if options.contains(&SearchReturn::Count) {
                    out.raw(" COUNT 0");
                }
                out.raw("\r\n");
            }
        }
        self.send(&out.bytes).await.map_err(io_error)?;
        self.refresh(uid).await.map_err(io_error)?;
        Ok(format!("{tag} OK Search completed\r\n"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch(
        &mut self,
        tag: &str,
        uid: bool,
        set: SequenceSet,
        mut items: Vec<FetchItem>,
        changed_since: Option<u64>,
        vanished: bool,
    ) -> Result<String, StoreError> {
        let account = self.account_id();
        if changed_since.is_some() {
            self.condstore = true;
            if !items.contains(&FetchItem::ModSeq) {
                items.push(FetchItem::ModSeq);
            }
        }
        if items.contains(&FetchItem::ModSeq) {
            self.condstore = true;
        }
        if uid && !items.contains(&FetchItem::Uid) {
            items.insert(0, FetchItem::Uid);
        }
        let selected = self.selected();
        let mailbox_id = selected.mailbox_id;
        let read_only = selected.read_only;
        let mut out = Out::new(self.utf8);

        if vanished && let Some(since) = changed_since {
            let largest = selected.largest_uid();
            let mut gone = self.store.imap_vanished(account, mailbox_id, since).await?;
            gone.retain(|uid| set.contains(*uid, largest.max(*uid)));
            if !gone.is_empty() {
                out.raw(&format!("* VANISHED (EARLIER) {}\r\n", response::sequence_set(&gone)));
            }
        }

        let selected = self.selected();
        let targets: Vec<(usize, u32)> = selected
            .resolve(&set, uid)
            .into_iter()
            .filter(|index| !selected.messages[*index].expunged)
            .map(|index| (index, selected.messages[index].uid))
            .collect();
        let needs_blob = items.iter().any(|item| {
            matches!(
                item,
                FetchItem::Envelope
                    | FetchItem::Rfc822
                    | FetchItem::Rfc822Header
                    | FetchItem::Rfc822Text
                    | FetchItem::Body
                    | FetchItem::BodyStructure
                    | FetchItem::BodySection { .. }
            )
        });
        let marks_seen = !read_only
            && items.iter().any(|item| {
                matches!(item, FetchItem::Rfc822 | FetchItem::Rfc822Text | FetchItem::BodySection { peek: false, .. })
            });

        let emails = self.store.imap_emails(account, mailbox_id, targets.iter().map(|(_, uid)| *uid).collect()).await?;
        let mut by_uid: BTreeMap<u32, ImapEmail> = emails.into_iter().map(|email| (email.uid, email)).collect();

        if marks_seen {
            let unseen: Vec<u32> =
                by_uid.values().filter(|email| !email.keywords.iter().any(|k| k == "$seen")).map(|e| e.uid).collect();
            if !unseen.is_empty() {
                self.store
                    .imap_store_flags(account, mailbox_id, unseen.clone(), FlagChange::Add(vec!["$seen".into()]), None)
                    .await?;
                for email in self.store.imap_emails(account, mailbox_id, unseen).await? {
                    by_uid.insert(email.uid, email);
                }
                if !items.contains(&FetchItem::Flags) {
                    items.push(FetchItem::Flags);
                }
            }
        }

        let condstore = self.condstore;
        for (index, uid) in targets {
            let Some(email) = by_uid.get(&uid) else { continue };
            if changed_since.is_some_and(|since| email.modseq <= since) {
                continue;
            }
            let raw = if needs_blob { Some(self.store.blob(&email.blob).await?) } else { None };
            let root = raw.as_deref().map(mime::parse);
            out.raw(&format!("* {} FETCH (", index + 1));
            let mut flags_sent = false;
            let mut modseq_sent = false;
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.raw(" ");
                }
                match item {
                    FetchItem::Uid => {
                        out.raw(&format!("UID {}", email.uid));
                    }
                    FetchItem::Flags => {
                        flags_sent = true;
                        out.raw(&format!("FLAGS {}", response::flags(&email.keywords)));
                    }
                    FetchItem::ModSeq => {
                        modseq_sent = true;
                        out.raw(&format!("MODSEQ ({})", email.modseq));
                    }
                    FetchItem::InternalDate => {
                        out.raw(&format!("INTERNALDATE {}", response::internal_date(email.received_at)));
                    }
                    FetchItem::Rfc822Size => {
                        out.raw(&format!("RFC822.SIZE {}", email.size));
                    }
                    FetchItem::Envelope => {
                        let (raw, root) = (raw.as_deref().unwrap_or_default(), root.as_ref().expect("parsed"));
                        out.raw("ENVELOPE ");
                        response::envelope(&mut out, raw, &root.header);
                    }
                    FetchItem::Body | FetchItem::BodyStructure => {
                        let (raw, root) = (raw.as_deref().unwrap_or_default(), root.as_ref().expect("parsed"));
                        let extended = matches!(item, FetchItem::BodyStructure);
                        out.raw(if extended { "BODYSTRUCTURE " } else { "BODY " });
                        response::body_structure(&mut out, raw, root, extended);
                    }
                    FetchItem::Rfc822 | FetchItem::Rfc822Header | FetchItem::Rfc822Text => {
                        let (raw, root) = (raw.as_deref().unwrap_or_default(), root.as_ref().expect("parsed"));
                        let (label, bytes) = match item {
                            FetchItem::Rfc822 => ("RFC822", raw),
                            FetchItem::Rfc822Header => ("RFC822.HEADER", &raw[root.header.clone()]),
                            _ => ("RFC822.TEXT", &raw[root.body.clone()]),
                        };
                        out.raw(label).raw(" ").literal(bytes);
                    }
                    FetchItem::BodySection { section, partial, .. } => {
                        let (raw, root) = (raw.as_deref().unwrap_or_default(), root.as_ref().expect("parsed"));
                        let bytes = response::section_bytes(raw, root, section).unwrap_or_default();
                        let (bytes, origin) = match partial {
                            Some((origin, count)) => {
                                let start = (*origin as usize).min(bytes.len());
                                let end = start.saturating_add(*count as usize).min(bytes.len());
                                (bytes[start..end].to_vec(), Some(*origin))
                            }
                            None => (bytes, None),
                        };
                        out.raw(&response::section_label(section, origin)).raw(" ").literal(&bytes);
                    }
                }
            }
            if condstore && !modseq_sent && flags_sent {
                out.raw(&format!(" MODSEQ ({})", email.modseq));
            }
            out.raw(")\r\n");
            if let Some(selected) = self.selected.as_mut()
                && let Some(known) = selected.messages.get_mut(index)
                && known.uid == email.uid
            {
                known.keywords = email.keywords.clone();
                known.modseq = email.modseq;
            }
            if out.bytes.len() > 256 * 1024 {
                self.send(&out.bytes).await.map_err(io_error)?;
                out.bytes.clear();
            }
        }
        self.send(&out.bytes).await.map_err(io_error)?;
        self.refresh(uid).await.map_err(io_error)?;
        Ok(format!("{tag} OK Fetch completed\r\n"))
    }

    #[allow(clippy::too_many_arguments)]
    async fn store_flags(
        &mut self,
        tag: &str,
        uid: bool,
        set: SequenceSet,
        unchanged_since: Option<u64>,
        action: StoreAction,
        silent: bool,
        flags: Vec<String>,
    ) -> Result<String, StoreError> {
        let selected = self.selected();
        if selected.read_only {
            return Ok(no(tag, Some("CANNOT"), "The mailbox is read-only"));
        }
        let mut keywords = Vec::new();
        for flag in &flags {
            if flag.eq_ignore_ascii_case("\\Recent") {
                continue;
            }
            match parser::keyword_of_flag(flag) {
                Some(keyword) => keywords.push(keyword),
                None => return Ok(format!("{tag} BAD {flag} cannot be stored\r\n")),
            }
        }
        if unchanged_since.is_some() {
            self.condstore = true;
        }
        let selected = self.selected();
        let mailbox_id = selected.mailbox_id;
        let targets: Vec<(usize, u32)> = selected
            .resolve(&set, uid)
            .into_iter()
            .filter(|index| !selected.messages[*index].expunged)
            .map(|index| (index, selected.messages[index].uid))
            .collect();
        let change = match action {
            StoreAction::Add => FlagChange::Add(keywords),
            StoreAction::Remove => FlagChange::Remove(keywords),
            StoreAction::Replace => FlagChange::Replace(keywords),
        };
        let account = self.account_id();
        let uids: Vec<u32> = targets.iter().map(|(_, uid)| *uid).collect();
        let skipped = self.store.imap_store_flags(account, mailbox_id, uids.clone(), change, unchanged_since).await?;
        let emails = self.store.imap_emails(account, mailbox_id, uids).await?;
        let by_uid: HashMap<u32, &ImapEmail> = emails.iter().map(|email| (email.uid, email)).collect();

        let condstore = self.condstore;
        let mut out = Out::new(self.utf8);
        let selected = self.selected.as_mut().expect("selected");
        for (index, message_uid) in targets {
            let Some(email) = by_uid.get(&message_uid) else { continue };
            let known = &mut selected.messages[index];
            let changed = known.keywords != email.keywords || known.modseq != email.modseq;
            known.keywords = email.keywords.clone();
            known.modseq = email.modseq;
            if skipped.contains(&message_uid) || (silent && !(condstore && changed)) {
                continue;
            }
            out.raw(&format!("* {} FETCH (", index + 1));
            if !silent {
                out.raw(&format!("FLAGS {}", response::flags(&email.keywords)));
            }
            if uid || condstore {
                out.raw(&format!("{}UID {}", if silent { "" } else { " " }, email.uid));
            }
            if condstore {
                out.raw(&format!(" MODSEQ ({})", email.modseq));
            }
            out.raw(")\r\n");
        }
        self.send(&out.bytes).await.map_err(io_error)?;
        self.refresh(false).await.map_err(io_error)?;
        if skipped.is_empty() {
            Ok(format!("{tag} OK Store completed\r\n"))
        } else {
            let numbers: Vec<u32> = if uid {
                skipped
            } else {
                let selected = self.selected();
                let mut positions: Vec<u32> =
                    skipped.iter().filter_map(|uid| selected.position(*uid)).map(|i| i as u32 + 1).collect();
                positions.sort_unstable();
                positions
            };
            Ok(format!("{tag} OK [MODIFIED {}] Conditional store failed\r\n", response::sequence_set(&numbers)))
        }
    }

    async fn copy(
        &mut self,
        tag: &str,
        uid: bool,
        set: SequenceSet,
        path: &str,
        remove_source: bool,
    ) -> Result<String, StoreError> {
        let selected = self.selected();
        if remove_source && selected.read_only {
            return Ok(no(tag, Some("CANNOT"), "The mailbox is read-only"));
        }
        let mailbox_id = selected.mailbox_id;
        let uids: Vec<u32> = selected
            .resolve(&set, uid)
            .into_iter()
            .filter(|index| !selected.messages[*index].expunged)
            .map(|index| selected.messages[index].uid)
            .collect();
        let named = self.named().await?;
        let Some(target) = mailboxes::find(&named, path) else {
            return Ok(no(tag, Some("TRYCREATE"), "No such mailbox"));
        };
        let account = self.account_id();
        let pairs = self.store.imap_copy(account, mailbox_id, uids, target.mailbox.id, remove_source).await?;
        let code = if pairs.is_empty() {
            String::new()
        } else {
            let (sources, targets): (Vec<u32>, Vec<u32>) = pairs.iter().copied().unzip();
            format!("[COPYUID {} {} {}] ", target.mailbox.uid_validity, uid_list(&sources), uid_list(&targets))
        };
        if !remove_source {
            self.refresh(uid).await.map_err(io_error)?;
            return Ok(format!("{tag} OK {code}Copy completed\r\n"));
        }
        if target.mailbox.id != mailbox_id {
            if !code.is_empty() {
                self.send(format!("* OK {code}Moved\r\n").as_bytes()).await.map_err(io_error)?;
            }
            let moved: Vec<u32> = pairs.iter().map(|(source, _)| *source).collect();
            self.report_removed(&moved).await.map_err(io_error)?;
        }
        self.refresh(true).await.map_err(io_error)?;
        Ok(format!("{tag} OK Move completed\r\n"))
    }
}

/// COPYUID lists keep the order of the pairs, so ranges are only used when they are in order.
fn uid_list(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    if sorted == uids {
        response::sequence_set(uids)
    } else {
        uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
    }
}

/// Reads the client's `DONE` while idling. Cancel-safe: bytes read before a wakeup stay in `line`.
async fn read_idle_line<R: AsyncRead + Unpin>(reader: &mut BufReader<R>, line: &mut Vec<u8>) -> io::Result<usize> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(0);
        }
        let (take, complete) = match available.iter().position(|&b| b == b'\n') {
            Some(end) => (end + 1, true),
            None => (available.len(), false),
        };
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if complete {
            return Ok(line.len());
        }
        if line.len() > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Line too long"));
        }
    }
}

fn io_error(err: io::Error) -> StoreError {
    StoreError::Internal(err.to_string())
}

fn is_system_keyword(keyword: &str) -> bool {
    ["$seen", "$answered", "$flagged", "$draft", DELETED_KEYWORD].contains(&keyword)
}

fn space_list(items: &[String]) -> String {
    items.iter().map(|item| format!(" {item}")).collect()
}

fn status_items(items: &[StatusItem], status: &uwumail_store::ImapStatus) -> String {
    items
        .iter()
        .map(|item| match item {
            StatusItem::Messages => format!("MESSAGES {}", status.messages),
            StatusItem::Recent => "RECENT 0".to_owned(),
            StatusItem::UidNext => format!("UIDNEXT {}", status.uid_next),
            StatusItem::UidValidity => format!("UIDVALIDITY {}", status.uid_validity),
            StatusItem::Unseen => format!("UNSEEN {}", status.unseen),
            StatusItem::Size => format!("SIZE {}", status.size),
            StatusItem::Deleted => format!("DELETED {}", status.deleted),
            StatusItem::HighestModSeq => format!("HIGHESTMODSEQ {}", status.highest_modseq),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or_default()
}
