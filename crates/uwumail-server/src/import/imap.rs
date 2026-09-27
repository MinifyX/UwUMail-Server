//! Copying mail from another server over IMAP (implicit TLS), folder by folder, with flags and the
//! date it arrived. Each folder remembers the last UID taken over, so running it again only fetches
//! what arrived since: copy once early, then again right before the switch.
//!
//! With a dovecot master user (mailcow's `DOVECOT_MASTER_USER`), nobody's password is needed:
//! the login is `person*master` with the master password.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use rustls_pki_types::ServerName;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use uwumail_store::{BlobHash, ImportProgress, IngestRequest, MailboxRole, MailboxTarget, Store, StoreError};

/// Messages fetched per request.
const BATCH: usize = 25;
const TIMEOUT: Duration = Duration::from_secs(120);
/// The largest literal this reads before allocating for it. A hostile or broken provider could
/// otherwise announce something like `{9223372036854775808}` and make the allocation abort the
/// whole server, which then crash-loops on the same account (security-audit-0.5.2 S-25). It also
/// bounds a fetched message body, since that arrives as a literal.
const MAX_LITERAL: usize = 64 * 1024 * 1024;
/// The longest response line this reads. A line grows until its newline comes, so without a limit a
/// provider that never sends one fills the memory (security-audit-0.8.0 T-7). Real lines are short;
/// the longest are `SEARCH` answers, a dozen bytes per message.
const MAX_LINE: usize = 16 * 1024 * 1024;
/// What one command's answers may add up to, lines and literals together: a full batch of the
/// largest messages and room besides. Without it a provider could keep answering for ever.
const MAX_ANSWER: usize = BATCH * MAX_LITERAL + 4 * MAX_LINE;

/// Where to copy from and how to log in.
pub struct Source {
    /// `host:port`, usually port 993.
    pub address: String,
    /// The name the certificate is checked against; the host of `address` when left out.
    pub tls_name: Option<String>,
    /// Extra certificate authorities, e.g. for tests.
    pub roots: Option<rustls::RootCertStore>,
    pub master_user: Option<String>,
    pub password: String,
    /// How to get there, when not straight: the admin can send fetching through the VPN.
    pub dialer: Option<uwumail_smtp::egress::Dialer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Atom(String),
    String(Vec<u8>),
    Nil,
    Open,
    Close,
}

impl Token {
    pub(crate) fn text(&self) -> Option<String> {
        match self {
            Token::Atom(atom) => Some(atom.clone()),
            Token::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        }
    }
}

/// One untagged or tagged response with its literals in place.
#[derive(Debug, Default)]
pub(crate) struct Response {
    pub(crate) tokens: Vec<Token>,
    /// The response without literals, for status codes like `[UIDVALIDITY 7]`.
    pub(crate) text: String,
}

/// Splits one segment of a response line into tokens. A trailing `{n}` announces a literal.
fn tokenize(segment: &[u8], tokens: &mut Vec<Token>) -> Option<usize> {
    let mut i = 0;
    while i < segment.len() {
        match segment[i] {
            b' ' | b'\r' | b'\n' => i += 1,
            b'(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            b'"' => {
                let mut value = Vec::new();
                i += 1;
                while i < segment.len() && segment[i] != b'"' {
                    if segment[i] == b'\\' && i + 1 < segment.len() {
                        i += 1;
                    }
                    value.push(segment[i]);
                    i += 1;
                }
                i += 1;
                tokens.push(Token::String(value));
            }
            b'{' => {
                let end = segment[i..].iter().position(|b| *b == b'}')? + i;
                let digits = std::str::from_utf8(&segment[i + 1..end]).ok()?.trim_end_matches('+');
                return digits.parse().ok();
            }
            _ => {
                let start = i;
                while i < segment.len() && !b" ()\"\r\n".contains(&segment[i]) {
                    i += 1;
                }
                let atom = String::from_utf8_lossy(&segment[start..i]).into_owned();
                tokens.push(if atom.eq_ignore_ascii_case("NIL") { Token::Nil } else { Token::Atom(atom) });
            }
        }
    }
    None
}

pub(crate) struct Connection {
    stream: BufReader<TlsStream<TcpStream>>,
    next_tag: u32,
}

impl Connection {
    pub(crate) async fn open(source: &Source) -> anyhow::Result<Connection> {
        let host = source.address.rsplit_once(':').map_or(source.address.as_str(), |(host, _)| host);
        let name = source.tls_name.clone().unwrap_or_else(|| host.trim_matches(['[', ']']).to_owned());
        let roots = source
            .roots
            .clone()
            .unwrap_or_else(|| rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
        let config =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(roots)
                .with_no_client_auth();
        let connect = async {
            match &source.dialer {
                Some(dialer) => {
                    let (host, port) = source
                        .address
                        .rsplit_once(':')
                        .and_then(|(host, port)| Some((host.trim_matches(['[', ']']), port.parse::<u16>().ok()?)))
                        .ok_or_else(|| std::io::Error::other("the address has no port"))?;
                    dialer.connect(host, port).await
                }
                None => TcpStream::connect(&source.address).await,
            }
        };
        let tcp = tokio::time::timeout(TIMEOUT, connect)
            .await
            .context("connecting timed out")?
            .with_context(|| format!("connecting to {}", source.address))?;
        let server_name = ServerName::try_from(name.clone()).map_err(|_| anyhow!("{name} is not a valid TLS name"))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(server_name, tcp)
            .await
            .with_context(|| format!("TLS with {} (checked as {name})", source.address))?;
        let mut connection = Connection { stream: BufReader::new(tls), next_tag: 1 };
        let mut budget = MAX_ANSWER;
        let greeting = read_response(&mut connection.stream, &mut budget).await?;
        if !greeting.text.starts_with("* OK") {
            bail!("the server did not greet: {}", greeting.text);
        }
        Ok(connection)
    }

    /// Sends a command and returns its untagged responses once it completed.
    pub(crate) async fn command(&mut self, command: &str) -> anyhow::Result<Vec<Response>> {
        let tag = format!("u{}", self.next_tag);
        self.next_tag += 1;
        self.stream.get_mut().write_all(format!("{tag} {command}\r\n").as_bytes()).await?;
        let mut untagged = Vec::new();
        let mut budget = MAX_ANSWER;
        loop {
            let response = read_response(&mut self.stream, &mut budget).await?;
            if let Some(status) = response.text.strip_prefix(&format!("{tag} ")) {
                if status.starts_with("OK") {
                    return Ok(untagged);
                }
                let shown = if command.starts_with("LOGIN") { "LOGIN" } else { command };
                bail!("{shown}: {status}");
            }
            untagged.push(response);
        }
    }
}

/// Reads one response with its literals, taking what it reads off `budget`.
async fn read_response<R: AsyncBufRead + Unpin>(stream: &mut R, budget: &mut usize) -> anyhow::Result<Response> {
    let mut response = Response::default();
    loop {
        let mut line = Vec::new();
        let limit = MAX_LINE.min(*budget) as u64 + 1;
        let read = tokio::time::timeout(TIMEOUT, (&mut *stream).take(limit).read_until(b'\n', &mut line))
            .await
            .context("the server stopped answering")??;
        if read == 0 {
            bail!("the server closed the connection");
        }
        // One byte more than allowed was let through, so the limit shows.
        if read as u64 == limit {
            bail!("the server sent more than this reads for one answer");
        }
        *budget -= read;
        let literal = tokenize(&line, &mut response.tokens);
        let shown = match literal {
            Some(_) => &line[..line.iter().rposition(|b| *b == b'{').unwrap_or(line.len())],
            None => &line[..],
        };
        response.text.push_str(String::from_utf8_lossy(shown).trim_end_matches(['\r', '\n']));
        let Some(size) = literal else { return Ok(response) };
        if size > MAX_LITERAL {
            bail!("the server announced a {size}-byte literal, more than this reads at once");
        }
        if size > *budget {
            bail!("the server sent more than this reads for one answer");
        }
        *budget -= size;
        let mut bytes = vec![0; size];
        tokio::time::timeout(TIMEOUT, stream.read_exact(&mut bytes)).await.context("the server stopped sending")??;
        response.tokens.push(Token::String(bytes));
    }
}

/// One answer to LIST: `* LIST (attributes) delimiter name`.
#[derive(Debug, PartialEq, Eq)]
struct ListEntry {
    attributes: Vec<String>,
    /// Empty for NIL.
    delimiter: String,
    raw: String,
}

/// Reads one LIST answer; `None` for one of any other shape, which is passed over. The server is
/// whatever the person typed in: `* LIST` with nothing after it used to cut the tokens at 3..2,
/// which panicked, and since the fetch account stayed due the server crashed again right after
/// every start (security-audit-0.16.0 PLAT-1).
fn list_entry(tokens: &[Token]) -> Option<ListEntry> {
    if tokens.get(1) != Some(&Token::Atom("LIST".into())) || tokens.get(2) != Some(&Token::Open) {
        return None;
    }
    let close = 3 + tokens.get(3..)?.iter().position(|token| *token == Token::Close)?;
    Some(ListEntry {
        attributes: tokens[3..close].iter().filter_map(Token::text).collect(),
        delimiter: tokens.get(close + 1).and_then(Token::text).unwrap_or_default(),
        raw: tokens.get(close + 2).and_then(Token::text)?,
    })
}

pub(crate) fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A folder on the old server.
#[derive(Debug, Clone)]
pub(crate) struct Folder {
    /// As the server names it, for SELECT.
    pub(crate) raw: String,
    /// Decoded path segments.
    pub(crate) path: Vec<String>,
    pub(crate) role: Option<MailboxRole>,
}

fn role_of(attributes: &[String], path: &[String]) -> Option<MailboxRole> {
    for attribute in attributes {
        match attribute.to_ascii_lowercase().as_str() {
            "\\sent" => return Some(MailboxRole::Sent),
            "\\drafts" => return Some(MailboxRole::Drafts),
            "\\junk" => return Some(MailboxRole::Junk),
            "\\trash" => return Some(MailboxRole::Trash),
            "\\archive" => return Some(MailboxRole::Archive),
            _ => {}
        }
    }
    let [name] = path else { return None };
    match name.to_lowercase().as_str() {
        "inbox" => Some(MailboxRole::Inbox),
        "sent" | "sent items" | "sent messages" | "gesendet" | "gesendete elemente" | "gesendete objekte" => {
            Some(MailboxRole::Sent)
        }
        "drafts" | "entwürfe" => Some(MailboxRole::Drafts),
        "junk" | "spam" | "junk e-mail" => Some(MailboxRole::Junk),
        "trash" | "deleted items" | "deleted messages" | "papierkorb" | "gelöschte elemente" => {
            Some(MailboxRole::Trash)
        }
        "archive" | "archiv" => Some(MailboxRole::Archive),
        _ => None,
    }
}

/// The folders to copy: everything selectable in the person's own namespace.
pub(crate) async fn folders(connection: &mut Connection) -> anyhow::Result<Vec<Folder>> {
    let mut shared_prefixes = Vec::new();
    if let Ok(responses) = connection.command("NAMESPACE").await {
        for response in responses {
            // * NAMESPACE (("" "/")) (("Other/" "/")) (("Shared/" "/")): prefixes past the first group.
            let mut depth = 0;
            let mut group = 0;
            for token in response.tokens.iter().skip(2) {
                match token {
                    Token::Open => {
                        if depth == 0 {
                            group += 1;
                        }
                        depth += 1;
                    }
                    Token::Close => depth -= 1,
                    Token::Nil if depth == 0 => group += 1,
                    Token::String(prefix) if depth == 2 && group > 1 => {
                        let prefix = String::from_utf8_lossy(prefix).into_owned();
                        if !prefix.is_empty() && !shared_prefixes.contains(&prefix) {
                            shared_prefixes.push(prefix);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let mut found = Vec::new();
    for response in connection.command("LIST \"\" \"*\"").await? {
        let Some(ListEntry { attributes, delimiter, raw }) = list_entry(&response.tokens) else { continue };
        if attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Noselect") || a.eq_ignore_ascii_case("\\NonExistent")) {
            continue;
        }
        if shared_prefixes.iter().any(|prefix| raw.starts_with(prefix.as_str())) {
            continue;
        }
        let decoded = uwumail_imap::mutf7::decode(&raw).unwrap_or_else(|| raw.clone());
        let path: Vec<String> = if delimiter.is_empty() {
            vec![decoded]
        } else {
            decoded.split(delimiter.as_str()).map(str::to_owned).filter(|part| !part.is_empty()).collect()
        };
        if path.is_empty() {
            continue;
        }
        let role =
            if raw.eq_ignore_ascii_case("INBOX") { Some(MailboxRole::Inbox) } else { role_of(&attributes, &path) };
        found.push(Folder { raw, path, role });
    }
    // Parents before children, so they exist when a child is created.
    found.sort_by_key(|folder| folder.path.len());
    Ok(found)
}

/// Finds or creates the mailbox a folder goes into.
pub(crate) async fn mailbox_for(store: &Store, account_id: i64, folder: &Folder) -> anyhow::Result<i64> {
    let mailboxes = store.mailboxes(account_id).await?;
    if let Some(role) = folder.role
        && let Some(mailbox) = mailboxes.iter().find(|mailbox| mailbox.role == Some(role))
    {
        return Ok(mailbox.id);
    }
    let mut parent = None;
    for (depth, name) in folder.path.iter().enumerate() {
        let current = store.mailboxes(account_id).await?;
        let existing =
            current.iter().find(|mailbox| mailbox.parent_id == parent && mailbox.name.eq_ignore_ascii_case(name));
        parent = Some(match existing {
            Some(mailbox) => mailbox.id,
            None => {
                let role = if depth + 1 == folder.path.len() { folder.role } else { None };
                store.create_mailbox(account_id, name, parent, role, 0, true).await?
            }
        });
    }
    parent.ok_or_else(|| anyhow!("the folder {} has no name", folder.raw))
}

#[derive(Debug, Default)]
pub(crate) struct Fetched {
    pub(crate) uid: u32,
    pub(crate) flags: Vec<String>,
    pub(crate) internal_date: Option<i64>,
    pub(crate) body: Option<Vec<u8>>,
}

pub(crate) fn parse_fetch(response: &Response) -> Option<Fetched> {
    let tokens = &response.tokens;
    if tokens.get(2) != Some(&Token::Atom("FETCH".into())) {
        return None;
    }
    let mut fetched = Fetched::default();
    let mut i = 4;
    while i < tokens.len() {
        let Some(Token::Atom(key)) = tokens.get(i) else {
            i += 1;
            continue;
        };
        match key.to_ascii_uppercase().as_str() {
            "UID" => {
                fetched.uid = tokens.get(i + 1).and_then(Token::text)?.parse().ok()?;
                i += 2;
            }
            "FLAGS" => {
                i += 2;
                while let Some(token) = tokens.get(i) {
                    i += 1;
                    match token {
                        Token::Close => break,
                        other => fetched.flags.extend(other.text()),
                    }
                }
            }
            "INTERNALDATE" => {
                fetched.internal_date = tokens
                    .get(i + 1)
                    .and_then(Token::text)
                    .and_then(|date| uwumail_imap::parser::parse_date_time(&date));
                i += 2;
            }
            key if key.starts_with("BODY[") || key == "RFC822" => {
                if let Some(Token::String(bytes)) = tokens.get(i + 1) {
                    fetched.body = Some(bytes.clone());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    (fetched.uid > 0).then_some(fetched)
}

/// What copying one person's mail did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Copied {
    pub folders: usize,
    pub messages: usize,
    /// Messages the mailbox here held already, left out (only when asked to look).
    pub skipped: usize,
    pub bytes: usize,
}

/// What a copy tells whoever started it, as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEvent {
    /// Every folder was looked at: how many there are, and how many messages are new in them.
    Planned { folders: usize, messages: usize },
    /// The old server renumbered a folder, so it is copied again from the start.
    Renumbered { folder: String },
    /// A folder is next: its name there, its path here, and how many of its messages are new.
    Folder { name: String, path: String, new: usize, exists: usize },
    /// How far the copy got, after every portion and every folder.
    Progress(Copied),
}

/// Whoever started a copy hears of it here. The answer says whether to go on: `false` stops the
/// copy after the portion that was just stored.
pub type Report<'a> = &'a mut (dyn FnMut(CopyEvent) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send);

#[derive(Debug, Clone, Copy, Default)]
pub struct CopyOptions {
    /// Only count what would be copied.
    pub dry_run: bool,
    /// Leave out messages the mailbox here holds already: by their Message-ID, by their bytes when
    /// they have none. For a move, where the old provider may show one message in several folders
    /// (Gmail's labels) and mail may have come here some other way already.
    pub skip_known: bool,
    /// Stop after the first portion that ends past this; the next copy goes on from there.
    pub deadline: Option<tokio::time::Instant>,
}

/// How a copy ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyEnd {
    /// Everything there was is here.
    Finished,
    /// The deadline passed; there is more.
    OutOfTime,
    /// The one who started it said stop.
    Stopped,
}

/// A folder with what is new in it.
struct Planned {
    folder: Folder,
    uid_validity: u32,
    exists: usize,
    uids: Vec<u32>,
}

/// Looks at every folder: what is new in it since the last copy.
async fn plan(
    store: &Store,
    connection: &mut Connection,
    account_id: i64,
    source_name: &str,
    report: Report<'_>,
) -> anyhow::Result<Vec<Planned>> {
    let mut planned = Vec::new();
    for folder in folders(connection).await? {
        let responses = connection.command(&format!("EXAMINE {}", quoted(&folder.raw))).await?;
        let uid_validity = examined_status(&responses, "UIDVALIDITY").unwrap_or(0);
        let exists = examined_exists(&responses);
        let known = store.import_progress(account_id, source_name, &folder.raw).await?;
        let last_uid = match known {
            Some(known) if known.uid_validity == uid_validity => known.last_uid,
            Some(_) => {
                report(CopyEvent::Renumbered { folder: folder.raw.clone() }).await;
                0
            }
            None => 0,
        };
        let uids: Vec<u32> = if exists == 0 {
            Vec::new()
        } else {
            let mut uids: Vec<u32> = connection
                // The last UID came from the other server; u32::MAX would overflow
                // (security-audit-0.16.0 PANIC-I1).
                .command(&format!("UID SEARCH UID {}:*", last_uid.saturating_add(1)))
                .await?
                .iter()
                .filter(|response| response.tokens.get(1) == Some(&Token::Atom("SEARCH".into())))
                .flat_map(|response| response.tokens.iter().skip(2).filter_map(Token::text))
                .filter_map(|uid| uid.parse::<u32>().ok())
                .filter(|uid| *uid > last_uid)
                .collect();
            uids.sort_unstable();
            uids.dedup();
            uids
        };
        planned.push(Planned { folder, uid_validity, exists, uids });
    }
    // The inbox first: a message the old server shows in several folders comes into the first one
    // it is found in, when the mailbox here is asked to leave out what it holds already.
    planned.sort_by_key(|planned| (planned.folder.role != Some(MailboxRole::Inbox), planned.folder.path.len()));
    Ok(planned)
}

fn examined_status(responses: &[Response], code: &str) -> Option<u32> {
    responses.iter().find_map(|response| {
        let rest = response.text.split_once(&format!("[{code} "))?.1;
        rest.split(']').next()?.trim().parse::<u32>().ok()
    })
}

fn examined_exists(responses: &[Response]) -> usize {
    responses
        .iter()
        .find_map(|response| match &response.tokens[..] {
            [_, Token::Atom(count), Token::Atom(word), ..] if word.eq_ignore_ascii_case("EXISTS") => {
                count.parse::<usize>().ok()
            }
            _ => None,
        })
        .unwrap_or(0)
}

/// Copies every folder of a logged-in connection into `account_id` here, what is new since the
/// last copy from `source_name`, telling `report` how it goes.
///
/// A full mailbox here ends it with [`uwumail_store::StoreError::QuotaExceeded`] inside the error;
/// what was stored until then stays, and the next copy goes on from the last finished portion.
pub(crate) async fn copy_folders(
    store: &Store,
    connection: &mut Connection,
    account_id: i64,
    source_name: &str,
    options: CopyOptions,
    report: Report<'_>,
) -> anyhow::Result<(Copied, CopyEnd)> {
    let planned = plan(store, connection, account_id, source_name, report).await?;
    let messages = planned.iter().map(|planned| planned.uids.len()).sum();
    if !report(CopyEvent::Planned { folders: planned.len(), messages }).await {
        return Ok((Copied::default(), CopyEnd::Stopped));
    }
    let mut copied = Copied::default();
    for Planned { folder, uid_validity, exists, uids } in planned {
        if uids.is_empty() {
            copied.folders += 1;
            continue;
        }
        let event =
            CopyEvent::Folder { name: folder.raw.clone(), path: folder.path.join("/"), new: uids.len(), exists };
        if !report(event).await {
            return Ok((copied, CopyEnd::Stopped));
        }
        if options.dry_run {
            copied.folders += 1;
            copied.messages += uids.len();
            continue;
        }
        let responses = connection.command(&format!("EXAMINE {}", quoted(&folder.raw))).await?;
        if examined_status(&responses, "UIDVALIDITY").unwrap_or(0) != uid_validity {
            // Renumbered between looking and copying: the next copy sees it and starts it over.
            copied.folders += 1;
            continue;
        }
        let mailbox = mailbox_for(store, account_id, &folder).await?;
        for batch in uids.chunks(BATCH) {
            if options.deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return Ok((copied, CopyEnd::OutOfTime));
            }
            let set = batch.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let responses =
                connection.command(&format!("UID FETCH {set} (UID FLAGS INTERNALDATE BODY.PEEK[])")).await?;
            let mut by_uid: HashMap<u32, Fetched> =
                responses.iter().filter_map(parse_fetch).map(|fetched| (fetched.uid, fetched)).collect();
            for uid in batch {
                let Some(fetched) = by_uid.remove(uid) else { continue };
                let Some(body) = fetched.body else { continue };
                if fetched.flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Deleted")) {
                    continue;
                }
                if options.skip_known {
                    let message_id = uwumail_smtp::header_value(&body, "Message-ID");
                    if store.holds_message(account_id, message_id, BlobHash::of(&body)).await? {
                        copied.skipped += 1;
                        continue;
                    }
                }
                let keywords =
                    fetched.flags.iter().filter_map(|flag| uwumail_imap::parser::keyword_of_flag(flag)).collect();
                let size = body.len();
                let request = IngestRequest {
                    account_id,
                    raw: body,
                    mailboxes: vec![MailboxTarget::Id(mailbox)],
                    keywords,
                    received_at: fetched.internal_date,
                };
                match store.ingest(request).await {
                    Ok(_) => {}
                    // Nested too deep or made of too many parts to be read safely
                    // (uwumail_store::mime_limits): left out, and the move goes on with the rest.
                    Err(StoreError::Rule { code: "invalidEmail", message }) => {
                        tracing::warn!(uid, folder = %folder.raw, %message, "a message was left out");
                        continue;
                    }
                    Err(err) => {
                        return Err(
                            anyhow::Error::from(err).context(format!("storing message {uid} of {}", folder.raw))
                        );
                    }
                }
                copied.messages += 1;
                copied.bytes += size;
            }
            let last_uid = *batch.last().expect("chunks are never empty");
            store
                .set_import_progress(account_id, source_name, &folder.raw, ImportProgress { uid_validity, last_uid })
                .await?;
            if !report(CopyEvent::Progress(copied)).await {
                return Ok((copied, CopyEnd::Stopped));
            }
        }
        copied.folders += 1;
        if !report(CopyEvent::Progress(copied)).await {
            return Ok((copied, CopyEnd::Stopped));
        }
    }
    Ok((copied, CopyEnd::Finished))
}

/// Copies the mail of `login` on the old server into `account` here.
pub async fn copy_mail(
    store: &Store,
    source: &Source,
    login: &str,
    account: &str,
    dry_run: bool,
    progress: &mut (dyn FnMut(&str) + Send),
) -> anyhow::Result<Copied> {
    let account = store.account(account).await?.ok_or_else(|| anyhow!("{account} does not exist here"))?;
    let mut connection = Connection::open(source).await?;
    let user = match &source.master_user {
        Some(master) => format!("{login}*{master}"),
        None => login.to_owned(),
    };
    connection.command(&format!("LOGIN {} {}", quoted(&user), quoted(&source.password))).await?;

    let source_name = source.tls_name.clone().unwrap_or_else(|| source.address.clone());
    let mut report = |event: CopyEvent| -> Pin<Box<dyn Future<Output = bool> + Send>> {
        match event {
            CopyEvent::Renumbered { folder } => {
                progress(&format!("{folder}: the old server renumbered this folder, copying it again"));
            }
            CopyEvent::Folder { name, path, new, exists } => {
                progress(&format!("{name} → {path}: {new} new of {exists}"))
            }
            CopyEvent::Planned { .. } | CopyEvent::Progress(_) => {}
        }
        Box::pin(std::future::ready(true))
    };
    let options = CopyOptions { dry_run, ..CopyOptions::default() };
    let (copied, _) = copy_folders(store, &mut connection, account.id, &source_name, options, &mut report).await?;
    let _ = connection.command("LOGOUT").await;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(line: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokenize(line.as_bytes(), &mut tokens);
        tokens
    }

    #[test]
    fn list_answers_of_any_shape() {
        for broken in
            ["* LIST", "* LIST )", "* LIST foo", "* LIST (\\Noselect", "* LIST ()", "* LIST (\\HasChildren) \"/\""]
        {
            assert_eq!(list_entry(&tokens(broken)), None, "{broken}");
        }
        assert_eq!(
            list_entry(&tokens("* LIST (\\HasNoChildren \\Sent) \"/\" \"Gesendet\"")),
            Some(ListEntry {
                attributes: vec!["\\HasNoChildren".into(), "\\Sent".into()],
                delimiter: "/".into(),
                raw: "Gesendet".into()
            })
        );
        assert_eq!(list_entry(&tokens("* LIST () NIL INBOX")).map(|entry| entry.delimiter), Some(String::new()));
    }

    /// The readers of what a user-chosen IMAP server answers, over noise and mangled answers: none
    /// of them may panic (security-audit-0.16.0 PLAT-1).
    #[test]
    fn the_answer_readers_survive_nonsense() {
        const ANSWERS: &[&[u8]] = &[
            b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n",
            b"* 1 FETCH (UID 7 FLAGS (\\Seen) INTERNALDATE \"17-Sep-2026 10:00:00 +0200\" BODY[] {5}\r\n",
            b"* NAMESPACE ((\"\" \"/\")) NIL ((\"Shared/\" \"/\"))\r\n",
            b"* 3 EXISTS\r\n",
            b"* OK [UIDVALIDITY 7] ok\r\n",
        ];
        let mut state = 0x5eed_0016_0000_0001u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        for _ in 0..5_000 {
            let mut line = ANSWERS[next() as usize % ANSWERS.len()].to_vec();
            for _ in 0..1 + next() % 4 {
                let at = next() as usize % (line.len() + 1);
                match next() % 3 {
                    0 => line.truncate(at),
                    1 => line.insert(at, b"()\"{} \xc3\xa9"[next() as usize % 8]),
                    _ if at < line.len() => line[at] = next() as u8,
                    _ => {}
                }
            }
            let mut response = Response::default();
            let _ = tokenize(&line, &mut response.tokens);
            let _ = list_entry(&response.tokens);
            let _ = parse_fetch(&response);
            let _ = examined_exists(std::slice::from_ref(&response));
        }
    }

    #[test]
    fn a_huge_literal_is_past_the_cap_read_response_enforces() {
        // The provider can announce any size; read_response refuses one over MAX_LITERAL before it
        // would allocate for it (security-audit-0.5.2 S-25).
        let mut tokens = Vec::new();
        let size = tokenize(b"* OK {9223372036854775807}\r\n", &mut tokens).expect("a literal size");
        assert!(size > MAX_LITERAL);
    }

    /// security-audit-0.8.0 T-7: a line without an end, or answers without an end, are cut off at
    /// their limits instead of being kept in memory; ordinary answers read as before.
    #[tokio::test]
    async fn endless_answers_are_cut_off() {
        let mut budget = MAX_ANSWER;
        let mut answer = &b"* 1 FETCH (UID 7 BODY[] {5}\r\nhello)\r\n"[..];
        let response = read_response(&mut answer, &mut budget).await.unwrap();
        assert_eq!(response.text, "* 1 FETCH (UID 7 BODY[] )");
        assert!(response.tokens.contains(&Token::String(b"hello".to_vec())));
        assert_eq!(budget, MAX_ANSWER - 37);

        let mut endless = b"* OK ".to_vec();
        endless.resize(MAX_LINE + 10, b'x');
        let error = read_response(&mut &endless[..], &mut { MAX_ANSWER }).await.unwrap_err();
        assert!(error.to_string().contains("more than this reads"), "{error}");

        let many = "* 1 EXISTS\r\n".repeat(10);
        let mut stream = many.as_bytes();
        let mut budget = 30;
        assert!(read_response(&mut stream, &mut budget).await.is_ok());
        assert!(read_response(&mut stream, &mut budget).await.is_ok());
        assert!(read_response(&mut stream, &mut budget).await.is_err(), "the third passes what the answer may take");
        let mut literal = &b"* 1 FETCH (BODY[] {100}\r\n"[..];
        assert!(read_response(&mut literal, &mut 50).await.is_err(), "a literal past the budget is not read");
    }

    #[test]
    fn responses_with_literals_become_tokens() {
        let mut tokens = Vec::new();
        let literal = tokenize(
            b"* 3 FETCH (UID 17 FLAGS (\\Seen $Label1) INTERNALDATE \"17-Sep-2026 10:00:00 +0200\" BODY[] {5}\r\n",
            &mut tokens,
        );
        assert_eq!(literal, Some(5));
        tokens.push(Token::String(b"Hallo".to_vec()));
        tokenize(b")\r\n", &mut tokens);
        let fetched = parse_fetch(&Response { tokens, text: String::new() }).unwrap();
        assert_eq!(fetched.uid, 17);
        assert_eq!(fetched.flags, ["\\Seen", "$Label1"]);
        assert_eq!(fetched.body.as_deref(), Some(&b"Hallo"[..]));
        assert!(fetched.internal_date.is_some());
    }

    async fn store_with_person(dir: &std::path::Path, password: Option<&str>) -> (Store, i64) {
        let store = Store::open(dir).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let new = uwumail_store::NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: password.map(str::to_owned),
            role: uwumail_store::Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(new).await.unwrap().id;
        (store, id)
    }

    async fn deliver(store: &Store, account: i64, mailbox: MailboxTarget, subject: &str, keywords: &[&str]) {
        let raw = format!("From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: {subject}\r\n\r\nHallo\r\n");
        let request = IngestRequest {
            account_id: account,
            raw: raw.into_bytes(),
            mailboxes: vec![mailbox],
            keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
            received_at: Some(1_700_000_000),
        };
        store.ingest(request).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mail_is_copied_with_folders_and_flags_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_id) = store_with_person(&dir.path().join("old"), Some("katzenpfote-123")).await;
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), "Eins", &["$seen", "$flagged"]).await;
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Sent), "Gesendet", &["$seen"]).await;
        let projects = old.create_mailbox(old_id, "Projekte", None, None, 0, true).await.unwrap();
        let archive = old.create_mailbox(old_id, "Alt", Some(projects), None, 0, true).await.unwrap();
        deliver(&old, old_id, MailboxTarget::Id(archive), "Übergabe", &[]).await;

        let generated = rcgen::generate_simple_self_signed(vec!["imap.example.org".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(uwumail_imap::Imap::new(old.clone(), 1 << 20).serve(listener, Arc::new(tls), shutdown_rx));

        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        let source = |password: &str| Source {
            address: address.clone(),
            tls_name: Some("imap.example.org".into()),
            roots: Some(roots.clone()),
            master_user: None,
            password: password.into(),
            dialer: None,
        };
        let (new, new_id) = store_with_person(&dir.path().join("new"), None).await;
        let right = source("katzenpfote-123");
        let mut lines = Vec::new();
        let mut show = |line: &str| lines.push(line.to_owned());

        let wrong =
            copy_mail(&new, &source("falsch-falsch"), "mini@example.org", "mini@example.org", false, &mut show).await;
        assert!(wrong.is_err());
        let counted = copy_mail(&new, &right, "mini@example.org", "mini@example.org", true, &mut show);
        assert_eq!(counted.await.unwrap().messages, 3);
        assert_eq!(new.mailboxes(new_id).await.unwrap().len(), 6, "a dry run creates no folders");

        let copied = copy_mail(&new, &right, "mini@example.org", "mini@example.org", false, &mut show);
        assert_eq!(copied.await.unwrap().messages, 3);
        let mailboxes = new.mailboxes(new_id).await.unwrap();
        let inbox = mailboxes.iter().find(|mailbox| mailbox.role == Some(MailboxRole::Inbox)).unwrap();
        assert_eq!((inbox.total_emails, inbox.unread_emails), (1, 0), "flags come along");
        let sent = mailboxes.iter().find(|mailbox| mailbox.role == Some(MailboxRole::Sent)).unwrap();
        assert_eq!(sent.total_emails, 1);
        let parent = mailboxes.iter().find(|mailbox| mailbox.name == "Projekte").unwrap();
        let child = mailboxes.iter().find(|mailbox| mailbox.name == "Alt").unwrap();
        assert_eq!((child.parent_id, child.total_emails), (Some(parent.id), 1));
        let emails = new.emails_in_mailbox(inbox.id, 10).await.unwrap();
        assert_eq!(emails[0].subject, "Eins");

        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), "Zwei", &[]).await;
        let again = copy_mail(&new, &right, "mini@example.org", "mini@example.org", false, &mut show);
        assert_eq!(again.await.unwrap().messages, 1, "only what arrived since");
        let inbox = new.mailboxes(new_id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
        assert_eq!(inbox.unwrap().total_emails, 2);
    }

    #[test]
    fn folders_find_their_role() {
        let path = |name: &str| vec![name.to_owned()];
        assert_eq!(role_of(&["\\HasNoChildren".into(), "\\Sent".into()], &path("Gesendet")), Some(MailboxRole::Sent));
        assert_eq!(role_of(&[], &path("Papierkorb")), Some(MailboxRole::Trash));
        assert_eq!(role_of(&[], &["Archiv".into(), "2025".into()]), None);
        assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
