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
use uwumail_store::{BlobHash, DavKind, ImportProgress, IngestRequest, MailboxRole, MailboxTarget, Store, StoreError};

/// Messages fetched per request.
const BATCH: usize = 25;
const TIMEOUT: Duration = Duration::from_secs(120);
/// The longest one command's whole answer may take.
const COMMAND_LIMIT: Duration = Duration::from_secs(5 * 60);
/// The same for a batch of messages, which may be large.
const FETCH_LIMIT: Duration = Duration::from_secs(30 * 60);
/// The largest literal this reads before allocating for it. A hostile or broken provider could
/// otherwise announce something like `{9223372036854775808}` and make the allocation abort the
/// whole server, which then crash-loops on the same account (security-audit-0.5.2 S-25). It also
/// bounds a fetched message body, since that arrives as a literal.
pub(crate) const MAX_LITERAL: usize = 64 * 1024 * 1024;
/// The longest response line this reads. A line grows until its newline comes, so without a limit a
/// provider that never sends one fills the memory (security-audit-0.8.0 T-7). Real lines are short;
/// the longest are `SEARCH` answers, a dozen bytes per message.
const MAX_LINE: usize = 16 * 1024 * 1024;
/// What one command's answers may add up to, lines and literals together: two of the largest
/// messages and room besides. Without it a provider could keep answering for ever. Fetches ask for
/// less: what the provider said the messages take (see [`fetch_chunk`]).
const MAX_ANSWER: usize = 2 * MAX_LITERAL + 4 * MAX_LINE;
/// Messages are fetched in portions of about this many bytes (by their `RFC822.SIZE`); a larger
/// message comes alone.
const CHUNK_BYTES: usize = 16 * 1024 * 1024;
/// What every import together (the admin's moves, personal moves, fetched mailboxes) may hold of
/// fetched messages at once, in KiB: a hostile or broken provider, or many at once, cannot fill
/// the memory (security review 0.22 M-1).
const IMPORT_BUDGET_KIB: usize = 512 * 1024;
static IMPORT_BYTES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(IMPORT_BUDGET_KIB);
/// What the answers to any other command may add up to, in memory: folder lists, status lines and
/// searches, which for a folder of a million messages are a million tokens.
const MAX_SMALL_ANSWER: usize = 16 * MAX_LINE;
/// What one token costs in memory beyond its bytes: its place in the list (a 32-byte enum, twice
/// over while the list grows) and an allocation of its own. Charged against the budget, which
/// otherwise counted only the bytes on the wire: a line of `a a a …` turned 16 MiB into hundreds of
/// megabytes of tokens (security-audit-0.16.0 PLAT-3).
const TOKEN_COST: usize = 96;

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

impl Source {
    /// A provider a person named, for a fetched mailbox or a move: `host:port`, checked against the
    /// host's certificate. The connection always goes through a dialer, which resolves the name
    /// once and connects only to the public addresses it found: through the proxy when the admin
    /// routes fetching there, straight otherwise. Connecting by name instead would resolve it again
    /// and try every address in turn, and a name that also points at this machine or its network
    /// (or has come to since it was checked) would make the server knock there on the person's
    /// behalf (security-audit-0.5.2 S-10, security-audit-0.16.0 PLAT-2).
    pub(crate) fn remote(
        host: &str,
        port: u16,
        password: String,
        dialer: Option<uwumail_smtp::egress::Dialer>,
    ) -> Source {
        Source {
            address: format!("{host}:{port}"),
            tls_name: Some(host.to_owned()),
            roots: None,
            master_user: None,
            password,
            dialer: Some(dialer.unwrap_or_else(|| {
                uwumail_smtp::egress::Egress::direct().dialer(uwumail_smtp::egress::Purpose::Fetch)
            })),
        }
    }
}

/// Whether every address `host` resolves to is a public one. Said before anything else is done, so
/// a person who typed a local name hears why; the dialer that connects checks again.
pub(crate) async fn resolves_publicly(host: &str, port: u16) -> bool {
    match tokio::net::lookup_host((host, port)).await {
        Ok(found) => {
            let found: Vec<_> = found.collect();
            !found.is_empty() && found.iter().all(|address| uwumail_smtp::is_public(address.ip()))
        }
        Err(_) => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Atom(String),
    String(Vec<u8>),
    Nil,
    Open,
    Close,
    /// A literal of this many bytes that did not fit what the answer may still take, read past
    /// and dropped (only for fetches that ask for it, see [`Connection::fetch_within`]).
    Dropped(usize),
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

/// More tokens than an answer may hold.
#[derive(Debug)]
struct TooManyTokens;

/// Splits one segment of a response line into tokens. A trailing `{n}` announces a literal.
/// `tokens` never grows beyond `max`.
fn tokenize(segment: &[u8], tokens: &mut Vec<Token>, max: usize) -> Result<Option<usize>, TooManyTokens> {
    let mut i = 0;
    while i < segment.len() {
        if tokens.len() >= max && !matches!(segment[i], b' ' | b'\r' | b'\n' | b'{') {
            return Err(TooManyTokens);
        }
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
                let Some(end) = segment[i..].iter().position(|b| *b == b'}') else { return Ok(None) };
                let Ok(digits) = std::str::from_utf8(&segment[i + 1..i + end]) else { return Ok(None) };
                return Ok(digits.trim_end_matches('+').parse().ok());
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
    Ok(None)
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
        let handshake = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server_name, tcp);
        let tls = tokio::time::timeout(TIMEOUT, handshake)
            .await
            .context("the TLS handshake timed out")?
            .with_context(|| format!("TLS with {} (checked as {name})", source.address))?;
        let mut connection = Connection { stream: BufReader::new(tls), next_tag: 1 };
        let mut budget = MAX_SMALL_ANSWER;
        let greeting = read_response(&mut connection.stream, &mut budget).await?;
        if !greeting.text.starts_with("* OK") {
            bail!("the server did not greet: {}", greeting.text);
        }
        Ok(connection)
    }

    /// Sends a command and returns its untagged responses once it completed. Every line has
    /// [`TIMEOUT`] to come, and the whole answer [`COMMAND_LIMIT`] ([`FETCH_LIMIT`] for a batch of
    /// messages): an untagged line now and then must not keep a command going for ever.
    pub(crate) async fn command(&mut self, command: &str) -> anyhow::Result<Vec<Response>> {
        let budget = if command.starts_with("UID FETCH") { MAX_ANSWER } else { MAX_SMALL_ANSWER };
        self.command_within(command, budget).await
    }

    /// [`Connection::command`] with the answer limited to `budget` bytes.
    async fn command_within(&mut self, command: &str, budget: usize) -> anyhow::Result<Vec<Response>> {
        let limit = if command.starts_with("UID FETCH") { FETCH_LIMIT } else { COMMAND_LIMIT };
        let shown = if command.starts_with("LOGIN") { "LOGIN" } else { command }.to_owned();
        tokio::time::timeout(limit, self.command_untimed(command, budget, false))
            .await
            .map_err(|_| anyhow!("{shown}: the server did not finish answering in time"))?
    }

    /// A fetch limited to `budget` bytes in which a message body that does not fit is read past
    /// and left as [`Token::Dropped`] instead of failing the whole command: a provider whose
    /// `RFC822.SIZE` is an estimate (Exchange) must not stop the import at that message, and the
    /// connection stays in step for the next command (security review 0.22 MFIX-M1).
    async fn fetch_within(&mut self, command: &str, budget: usize) -> anyhow::Result<Vec<Response>> {
        tokio::time::timeout(FETCH_LIMIT, self.command_untimed(command, budget, true))
            .await
            .map_err(|_| anyhow!("{command}: the server did not finish answering in time"))?
    }

    /// Logs in with an OAuth access token as SASL XOAUTH2 (Microsoft, Google), the token in the
    /// command itself (SASL-IR). A refused token comes with a challenge first, which is answered with
    /// an empty line; the refusal after it is the error. The token never shows in an error.
    pub(crate) async fn authenticate_xoauth2(&mut self, user: &str, token: &str) -> anyhow::Result<()> {
        let command = format!("AUTHENTICATE XOAUTH2 {}", uwumail_smtp::provider_oauth::xoauth2(user, token));
        tokio::time::timeout(COMMAND_LIMIT, self.command_untimed(&command, MAX_SMALL_ANSWER, false))
            .await
            .map_err(|_| anyhow!("AUTHENTICATE: the server did not finish answering in time"))??;
        Ok(())
    }

    async fn command_untimed(
        &mut self,
        command: &str,
        mut budget: usize,
        drop_large: bool,
    ) -> anyhow::Result<Vec<Response>> {
        let tag = format!("u{}", self.next_tag);
        self.next_tag += 1;
        self.stream.get_mut().write_all(format!("{tag} {command}\r\n").as_bytes()).await?;
        let authenticating = command.starts_with("AUTHENTICATE");
        let mut challenged = false;
        let mut untagged = Vec::new();
        loop {
            let response = read_response_dropping(&mut self.stream, &mut budget, drop_large).await?;
            if let Some(status) = response.text.strip_prefix(&format!("{tag} ")) {
                if status.starts_with("OK") {
                    return Ok(untagged);
                }
                let shown = if command.starts_with("LOGIN") {
                    "LOGIN"
                } else if authenticating {
                    "AUTHENTICATE"
                } else {
                    command
                };
                bail!("{shown}: {status}");
            }
            // XOAUTH2's error challenge: an empty answer, and the server says no.
            if authenticating && response.text.starts_with('+') {
                if challenged {
                    bail!("AUTHENTICATE: the server kept asking");
                }
                challenged = true;
                self.stream.get_mut().write_all(b"\r\n").await?;
                continue;
            }
            untagged.push(response);
        }
    }
}

/// Reads one response with its literals, taking what it holds of it off `budget`: the text, every
/// token and the literals.
async fn read_response<R: AsyncBufRead + Unpin>(stream: &mut R, budget: &mut usize) -> anyhow::Result<Response> {
    read_response_dropping(stream, budget, false).await
}

/// [`read_response`]; with `drop_large`, a literal that does not fit `budget` (but is no larger
/// than twice [`MAX_LITERAL`]) is read past without being kept and becomes [`Token::Dropped`].
async fn read_response_dropping<R: AsyncBufRead + Unpin>(
    stream: &mut R,
    budget: &mut usize,
    drop_large: bool,
) -> anyhow::Result<Response> {
    let too_much = || anyhow!("the server sent more than this reads for one answer");
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
        // The line is kept twice: as its text and as tokens, each of which costs its own place.
        let charge = |bytes: usize| bytes.checked_mul(2);
        *budget = charge(read).and_then(|cost| budget.checked_sub(cost)).ok_or_else(too_much)?;
        let before = response.tokens.len();
        let max = before + *budget / TOKEN_COST;
        let literal = tokenize(&line, &mut response.tokens, max).map_err(|_| too_much())?;
        *budget -= (response.tokens.len() - before) * TOKEN_COST;
        let shown = match literal {
            Some(_) => &line[..line.iter().rposition(|b| *b == b'{').unwrap_or(line.len())],
            None => &line[..],
        };
        response.text.push_str(String::from_utf8_lossy(shown).trim_end_matches(['\r', '\n']));
        let Some(size) = literal else { return Ok(response) };
        // Only a literal of at most MAX_LITERAL is ever kept. Read past and dropped, one may be
        // larger (up to twice that): a message whose size the provider understated is skipped
        // instead of stopping the import (security review 0.22 MFIX-M1, MFIX2-H1).
        if size > MAX_LITERAL && !(drop_large && size <= 2 * MAX_LITERAL) {
            bail!("the server announced a {size}-byte literal, more than this reads at once");
        }
        if size > *budget || size > MAX_LITERAL {
            if !drop_large {
                bail!("the server sent more than this reads for one answer");
            }
            // The token it leaves is charged like any other.
            *budget = budget.checked_sub(TOKEN_COST).ok_or_else(too_much)?;
            let mut sink = tokio::io::sink();
            let read = tokio::time::timeout(TIMEOUT, tokio::io::copy(&mut (&mut *stream).take(size as u64), &mut sink))
                .await
                .context("the server stopped sending")??;
            if read as usize != size {
                bail!("the server closed the connection");
            }
            response.tokens.push(Token::Dropped(size));
            continue;
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
        // Stored names are trimmed, so "Kunden " must find the "Kunden" its parent folder became.
        let name = name.trim();
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
    /// `RFC822.SIZE`, when asked for.
    pub(crate) size: Option<usize>,
    /// The body was left out: larger than this server takes, or than the fetch could take
    /// (see [`fetch_chunk`]).
    pub(crate) dropped: bool,
    /// The body was larger than the answer could still take and was read past.
    pub(crate) read_past: bool,
}

/// How a batch of messages is fetched: in portions of about [`CHUNK_BYTES`], each message with
/// the size the provider gave, and the ones larger than this server takes.
#[derive(Debug, Default)]
pub(crate) struct FetchPlan {
    pub(crate) chunks: Vec<Vec<(u32, usize)>>,
    pub(crate) too_large: Vec<u32>,
}

/// Asks the provider how large the messages are (`RFC822.SIZE`) and plans their fetching: larger
/// ones alone, ones above `max_size` not at all (security review 0.22 M-1). A message the answer
/// leaves out is planned at `max_size`, alone.
pub(crate) async fn plan_fetch(
    connection: &mut Connection,
    uids: &[u32],
    max_size: usize,
) -> anyhow::Result<FetchPlan> {
    let set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let sizes: HashMap<u32, usize> = connection
        .command_within(&format!("UID FETCH {set} (UID RFC822.SIZE)"), MAX_SMALL_ANSWER)
        .await?
        .into_iter()
        .filter_map(parse_fetch)
        .filter_map(|fetched| Some((fetched.uid, fetched.size?)))
        .collect();
    let mut plan = FetchPlan::default();
    let mut planned = Vec::new();
    for &uid in uids {
        let size = sizes.get(&uid).copied().unwrap_or(max_size);
        if size > max_size {
            plan.too_large.push(uid);
        } else {
            planned.push((uid, size));
        }
    }
    plan.chunks = chunks_of(&planned);
    Ok(plan)
}

/// Groups messages with their sizes, in order, into portions of about [`CHUNK_BYTES`] and at most
/// [`BATCH`]; a larger message comes alone.
fn chunks_of(messages: &[(u32, usize)]) -> Vec<Vec<(u32, usize)>> {
    let mut chunks = Vec::new();
    let mut chunk: Vec<(u32, usize)> = Vec::new();
    let mut chunk_bytes = 0;
    for &(uid, size) in messages {
        if !chunk.is_empty() && (chunk_bytes + size > CHUNK_BYTES || chunk.len() >= BATCH) {
            chunks.push(std::mem::take(&mut chunk));
            chunk_bytes = 0;
        }
        chunk.push((uid, size));
        chunk_bytes += size;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// What a fetch of messages said to take `declared` bytes in all may read: sizes are not always
/// exact (some servers count line ends differently); flags, dates and the answer lines take room
/// too.
fn fetch_budget(declared: usize, messages: usize) -> usize {
    (declared + declared / 4 + messages * 64 * 1024 + 1024 * 1024).min(MAX_ANSWER)
}

/// Room in the import budget for `bytes`. Never more than the whole budget can be asked for: a
/// request above it is an error, not a smaller permit that would leave bytes uncounted.
async fn import_permit(bytes: usize) -> anyhow::Result<tokio::sync::SemaphorePermit<'static>> {
    let kib = bytes.div_ceil(1024);
    if kib > IMPORT_BUDGET_KIB {
        bail!("a fetch of {bytes} bytes is more than all imports together may hold");
    }
    IMPORT_BYTES.acquire_many(kib as u32).await.context("the import budget is closed")
}

/// Fetches the portions of a [`FetchPlan`] one after another; see [`fetch_chunk`].
pub(crate) struct FetchQueue {
    chunks: std::collections::VecDeque<Vec<(u32, usize)>>,
    max_size: usize,
}

/// What one step of a [`FetchQueue`] brought: the UIDs it dealt with, in order, the messages
/// among them, and the room in the import budget they take, held while they are stored.
pub(crate) struct FetchedPortion {
    pub(crate) uids: Vec<u32>,
    pub(crate) messages: Vec<Fetched>,
    pub(crate) _permit: tokio::sync::SemaphorePermit<'static>,
}

impl FetchQueue {
    pub(crate) fn new(chunks: Vec<Vec<(u32, usize)>>, max_size: usize) -> FetchQueue {
        FetchQueue { chunks: chunks.into(), max_size }
    }

    /// The next portion, `None` once all of them came. What a portion could not take yet goes
    /// back to the front of the queue, in UID order.
    pub(crate) async fn next(&mut self, connection: &mut Connection) -> anyhow::Result<Option<FetchedPortion>> {
        let Some(chunk) = self.chunks.pop_front() else { return Ok(None) };
        let (portion, rest) = fetch_chunk(connection, &chunk, self.max_size).await?;
        for chunk in rest.into_iter().rev() {
            self.chunks.push_front(chunk);
        }
        Ok(Some(portion))
    }
}

/// Fetches one portion once the server-wide import budget has room for it. The answer may only
/// take a little more than the sizes the provider gave, and nothing beyond that is held: one call
/// holds at most the room it counted.
///
/// A body larger than that (`RFC822.SIZE` is only an estimate at some providers, Exchange among
/// them) is read past. The portion then ends before it, and it and everything after it are handed
/// back to be fetched again, it with room for `max_size` (security review 0.22 MFIX-M1, MFIX2-H1).
/// A message that had that room and still did not fit, or whose body is larger than `max_size`,
/// comes back with [`Fetched::dropped`] and no body, for the caller to count as skipped. Answers
/// for UIDs that were not asked for are left out. Network and protocol errors fail as before.
async fn fetch_chunk(
    connection: &mut Connection,
    chunk: &[(u32, usize)],
    max_size: usize,
) -> anyhow::Result<(FetchedPortion, Vec<Vec<(u32, usize)>>)> {
    let declared: usize = chunk.iter().map(|(_, size)| size).sum();
    let budget = fetch_budget(declared, chunk.len());
    let permit = import_permit(budget).await?;
    let set = chunk.iter().map(|(uid, _)| uid.to_string()).collect::<Vec<_>>().join(",");
    let responses =
        connection.fetch_within(&format!("UID FETCH {set} (UID FLAGS INTERNALDATE BODY.PEEK[])"), budget).await?;
    let mut by_uid: HashMap<u32, Fetched> = HashMap::new();
    for mut fetched in responses.into_iter().filter_map(parse_fetch) {
        // Only what was asked for, and each message once.
        if !chunk.iter().any(|(uid, _)| *uid == fetched.uid) || by_uid.contains_key(&fetched.uid) {
            continue;
        }
        if fetched.body.as_ref().is_some_and(|body| body.len() > max_size) {
            fetched.body = None;
            fetched.dropped = true;
        }
        by_uid.insert(fetched.uid, fetched);
    }
    // The first message that was read past without having had room of its own ends the portion.
    // Each message read past is fetched again alone with room for `max_size`, the others in
    // portions as before, all in UID order. Alone with that room, being read past is final.
    let alone = chunk.len() == 1 && chunk[0].1 >= max_size;
    let read_past = |uid: &u32| by_uid.get(uid).is_some_and(|fetched| fetched.read_past);
    let cut = if alone { None } else { chunk.iter().position(|(uid, _)| read_past(uid)) };
    let (taken, after) = match cut {
        Some(at) => (&chunk[..at], &chunk[at..]),
        None => (chunk, &chunk[chunk.len()..]),
    };
    let mut rest = Vec::new();
    let mut run = Vec::new();
    for &(uid, size) in after {
        if read_past(&uid) {
            rest.extend(chunks_of(&std::mem::take(&mut run)));
            rest.push(vec![(uid, size.max(max_size))]);
        } else {
            run.push((uid, size));
        }
    }
    rest.extend(chunks_of(&run));
    let uids: Vec<u32> = taken.iter().map(|(uid, _)| *uid).collect();
    let messages = uids.iter().filter_map(|uid| by_uid.remove(uid)).collect();
    Ok((FetchedPortion { uids, messages, _permit: permit }, rest))
}

/// Reads one FETCH answer, taking the body out of it instead of copying it.
pub(crate) fn parse_fetch(mut response: Response) -> Option<Fetched> {
    let tokens = &mut response.tokens;
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
                match tokens.get_mut(i + 1) {
                    Some(Token::String(bytes)) => fetched.body = Some(std::mem::take(bytes)),
                    Some(Token::Dropped(_)) => {
                        fetched.dropped = true;
                        fetched.read_past = true;
                    }
                    _ => {}
                }
                i += 2;
            }
            "RFC822.SIZE" => {
                fetched.size = tokens.get(i + 1).and_then(Token::text).and_then(|size| size.parse().ok());
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
    /// Messages left out: ones the mailbox here held already (only when asked to look), and ones
    /// larger than this server takes.
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
    /// Contacts or calendar entries found in an IMAP folder that holds them as messages (Kolab
    /// and others keep them so), only for the kinds [`CopyOptions::contacts`] and
    /// [`CopyOptions::calendars`] asked for: the vCards or iCalendar texts of one portion, from
    /// messages that are nothing but such an object (see [`pure_object_texts`]).
    ///
    /// For this event the answer means *stored*, not *go on*: when it is `false` (the import
    /// failed, or nobody took them) the messages are copied as mail after all, so nothing is lost.
    Objects { folder: String, kind: DavKind, texts: Vec<String> },
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
    /// Look for contacts kept as messages in folders named so (see [`CopyEvent::Objects`]).
    pub contacts: bool,
    /// Look for calendar entries kept as messages in folders named so.
    pub calendars: bool,
    /// The largest message this server takes (`smtp.max_message_size`); larger ones are left out
    /// and counted as skipped. 0 means the largest a fetch reads at all.
    pub max_size: usize,
}

impl CopyOptions {
    fn max_size(&self) -> usize {
        if self.max_size == 0 { MAX_LITERAL } else { self.max_size.min(MAX_LITERAL) }
    }

    /// The kind of objects to take out of `folder`, when it is a folder of a kind asked for.
    fn objects_in(&self, folder: &Folder) -> Option<DavKind> {
        object_folder(folder).filter(|kind| match kind {
            DavKind::Addressbook => self.contacts,
            DavKind::Calendar => self.calendars,
        })
    }
}

/// Whether a folder is one where groupware servers keep contacts or calendar entries as messages
/// (Kolab, and some others): by its top folder's usual names.
pub(crate) fn object_folder(folder: &Folder) -> Option<DavKind> {
    let top = folder.path.first()?.to_lowercase();
    match top.as_str() {
        "contacts" | "kontakte" | "adressbuch" | "address book" | "addressbook" => Some(DavKind::Addressbook),
        "calendar" | "kalender" => Some(DavKind::Calendar),
        _ => None,
    }
}

/// The longest plain text a message may carry besides its object and still count as an object
/// (Kolab writes a short "This is a Kolab Groupware object" note).
const OBJECT_STUB_CHARS: usize = 400;

/// The vCards or iCalendar texts of a message that is nothing but a contact or calendar object,
/// as groupware servers (Kolab and others) keep them in IMAP folders. Empty for everything else:
/// an email with a text or HTML body, other attachments or a nested message is mail, even in a
/// folder named "Kalender" (an invitation someone filed there), and is copied as mail (security
/// review 0.22 MOV-1).
pub(crate) fn pure_object_texts(raw: &[u8], kind: DavKind) -> Vec<String> {
    use mail_parser::{MimeHeaders, PartType};
    let Some(message) = uwumail_store::mime_limits::parse_message(raw) else { return Vec::new() };
    // Kolab's header alone (anyone can write it) lifts nothing: its own format has to be there too.
    let kolab = message.header("X-Kolab-Type").is_some()
        && message.parts.iter().any(|part| {
            part.content_type().is_some_and(|ct| {
                ct.ctype().eq_ignore_ascii_case("application")
                    && ct.subtype().unwrap_or_default().to_ascii_lowercase().starts_with("x-vnd.kolab.")
            })
        });
    let mut texts = Vec::new();
    for part in &message.parts {
        let (ctype, subtype) = part
            .content_type()
            .map(|ct| (ct.ctype().to_ascii_lowercase(), ct.subtype().unwrap_or_default().to_ascii_lowercase()))
            .unwrap_or_else(|| ("text".into(), "plain".into()));
        let wanted = match kind {
            DavKind::Addressbook => {
                matches!((ctype.as_str(), subtype.as_str()), ("text", "vcard" | "x-vcard" | "directory"))
            }
            DavKind::Calendar => {
                matches!((ctype.as_str(), subtype.as_str()), ("text", "calendar") | ("application", "ics"))
            }
        };
        if wanted {
            let text = match &part.body {
                PartType::Text(text) => text.to_string(),
                PartType::Binary(bytes) | PartType::InlineBinary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => return Vec::new(),
            };
            // An iTIP message (an invitation, a reply) is mail someone sent; stored objects carry no
            // METHOD (security review 0.22 R2-INFO-2).
            let itip = part.content_type().is_some_and(|ct| ct.attribute("method").is_some())
                || text.lines().any(|line| line.trim_start().to_ascii_uppercase().starts_with("METHOD:"));
            if itip && kind == DavKind::Calendar {
                return Vec::new();
            }
            if !text.trim().is_empty() {
                texts.push(text);
            }
            continue;
        }
        match &part.body {
            PartType::Multipart(_) => {}
            // Kolab's own formats travel next to the object.
            _ if ctype == "application" && subtype.starts_with("x-vnd.kolab.") => {}
            // The short note next to the object, not a letter.
            PartType::Text(text)
                if ctype == "text"
                    && subtype == "plain"
                    && !part.content_disposition().is_some_and(|d| d.is_attachment())
                    && (kolab || text.trim().chars().count() <= OBJECT_STUB_CHARS) => {}
            _ => return Vec::new(),
        }
    }
    texts
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
        // Made when the first message goes in: a folder of contacts may hold none.
        let mut mailbox = None;
        let objects = options.objects_in(&folder);
        for batch in uids.chunks(BATCH) {
            if options.deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return Ok((copied, CopyEnd::OutOfTime));
            }
            let plan = plan_fetch(connection, batch, options.max_size()).await?;
            for uid in &plan.too_large {
                tracing::warn!(uid, folder = %folder.raw, "a message larger than this server takes was left out");
                copied.skipped += 1;
            }
            let mut queue = FetchQueue::new(plan.chunks, options.max_size());
            while let Some(FetchedPortion { uids: chunk, messages, _permit }) = queue.next(connection).await? {
                let mut by_uid: HashMap<u32, Fetched> = messages.into_iter().map(|f| (f.uid, f)).collect();
                let mut found_objects = Vec::new();
                // The messages the objects came in, copied as mail when the objects cannot be stored.
                let mut object_messages = Vec::new();
                for uid in &chunk {
                    let Some(fetched) = by_uid.remove(uid) else { continue };
                    if fetched.dropped {
                        tracing::warn!(uid, folder = %folder.raw, "a message larger than this server takes was left out");
                        copied.skipped += 1;
                        continue;
                    }
                    let Some(body) = fetched.body.as_deref() else { continue };
                    if fetched.flags.iter().any(|flag| flag.eq_ignore_ascii_case("\\Deleted")) {
                        continue;
                    }
                    // The provider said it was smaller.
                    if body.len() > options.max_size() {
                        tracing::warn!(uid, folder = %folder.raw, "a message larger than this server takes was left out");
                        copied.skipped += 1;
                        continue;
                    }
                    if let Some(kind) = objects {
                        let texts = pure_object_texts(body, kind);
                        if !texts.is_empty() {
                            found_objects.extend(texts);
                            object_messages.push(fetched);
                            continue;
                        }
                    }
                    copy_message(store, account_id, &folder, &mut mailbox, options, fetched, &mut copied).await?;
                }
                if let Some(kind) = objects
                    && !found_objects.is_empty()
                {
                    let event = CopyEvent::Objects { folder: folder.raw.clone(), kind, texts: found_objects };
                    if report(event).await {
                        for fetched in &object_messages {
                            copied.messages += 1;
                            copied.bytes += fetched.body.as_ref().map_or(0, Vec::len);
                        }
                    } else {
                        tracing::info!(folder = %folder.raw, "objects could not be stored, copying them as mail");
                        for fetched in object_messages {
                            copy_message(store, account_id, &folder, &mut mailbox, options, fetched, &mut copied)
                                .await?;
                        }
                    }
                }
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

/// Stores one fetched message as mail in the folder's mailbox here (made on first use).
async fn copy_message(
    store: &Store,
    account_id: i64,
    folder: &Folder,
    mailbox: &mut Option<i64>,
    options: CopyOptions,
    fetched: Fetched,
    copied: &mut Copied,
) -> anyhow::Result<()> {
    let Fetched { uid, flags, internal_date, body, .. } = fetched;
    let Some(body) = body else { return Ok(()) };
    let mailbox = match *mailbox {
        Some(mailbox) => mailbox,
        None => *mailbox.insert(mailbox_for(store, account_id, folder).await?),
    };
    if options.skip_known {
        let message_id = uwumail_smtp::header_value(&body, "Message-ID");
        if store.holds_message(account_id, message_id, BlobHash::of(&body)).await? {
            copied.skipped += 1;
            return Ok(());
        }
    }
    let keywords = flags.iter().filter_map(|flag| uwumail_imap::parser::keyword_of_flag(flag)).collect();
    let size = body.len();
    let request = IngestRequest {
        account_id,
        raw: body,
        mailboxes: vec![MailboxTarget::Id(mailbox)],
        keywords,
        received_at: internal_date,
    };
    match store.ingest(request).await {
        Ok(_) => {}
        // Nested too deep or made of too many parts to be read safely
        // (uwumail_store::mime_limits): left out, and the move goes on with the rest.
        Err(StoreError::Rule { code: "invalidEmail", message }) => {
            tracing::warn!(uid, folder = %folder.raw, %message, "a message was left out");
            return Ok(());
        }
        Err(err) => {
            return Err(anyhow::Error::from(err).context(format!("storing message {uid} of {}", folder.raw)));
        }
    }
    copied.messages += 1;
    copied.bytes += size;
    Ok(())
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
            CopyEvent::Planned { .. } | CopyEvent::Progress(_) | CopyEvent::Objects { .. } => {}
        }
        Box::pin(std::future::ready(true))
    };
    let options = CopyOptions { dry_run, ..CopyOptions::default() };
    let (copied, _) = copy_folders(store, &mut connection, account.id, &source_name, options, &mut report).await?;
    let _ = connection.command("LOGOUT").await;
    Ok(copied)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn tokens(line: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        tokenize(line.as_bytes(), &mut tokens, usize::MAX).unwrap();
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
            let _ = tokenize(&line, &mut response.tokens, 100_000);
            let _ = list_entry(&response.tokens);
            let _ = examined_exists(std::slice::from_ref(&response));
            let _ = parse_fetch(response);
        }
    }

    #[test]
    fn a_huge_literal_is_past_the_cap_read_response_enforces() {
        // The provider can announce any size; read_response refuses one over MAX_LITERAL before it
        // would allocate for it (security-audit-0.5.2 S-25).
        let mut tokens = Vec::new();
        let size =
            tokenize(b"* OK {9223372036854775807}\r\n", &mut tokens, usize::MAX).unwrap().expect("a literal size");
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
        // The line twice, its eight tokens and the literal.
        assert_eq!(budget, MAX_ANSWER - 2 * 32 - 8 * TOKEN_COST - 5);

        let mut endless = b"* OK ".to_vec();
        endless.resize(MAX_LINE + 10, b'x');
        let error = read_response(&mut &endless[..], &mut { MAX_ANSWER }).await.unwrap_err();
        assert!(error.to_string().contains("more than this reads"), "{error}");

        let many = "* 1 EXISTS\r\n".repeat(10);
        let mut stream = many.as_bytes();
        let mut budget = 2 * (2 * 12 + 3 * TOKEN_COST) + 10;
        assert!(read_response(&mut stream, &mut budget).await.is_ok());
        assert!(read_response(&mut stream, &mut budget).await.is_ok());
        assert!(read_response(&mut stream, &mut budget).await.is_err(), "the third passes what the answer may take");
        let mut literal = &b"* 1 FETCH (BODY[] {100}\r\n"[..];
        assert!(read_response(&mut literal, &mut 50).await.is_err(), "a literal past the budget is not read");
    }

    /// security-audit-0.16.0 PLAT-3: what an answer holds is charged, not only what came over the
    /// wire. Lines of one-letter words are cut off long before their bytes would be.
    #[tokio::test]
    async fn many_small_words_are_charged_as_the_tokens_they_become() {
        let mut line = b"* ".to_vec();
        line.extend(b"a ".repeat(512 * 1024));
        line.extend(b"\r\n");
        let lines = line.repeat(64);
        let mut stream = &lines[..];
        let mut budget = MAX_SMALL_ANSWER;
        let mut read = 0;
        let error = loop {
            match read_response(&mut stream, &mut budget).await {
                Ok(_) => read += 1,
                Err(err) => break err,
            }
        };
        assert!(error.to_string().contains("more than this reads"), "{error}");
        // Each line of 1 MiB is half a million tokens, about 50 MiB held.
        assert!(read <= MAX_SMALL_ANSWER / (48 * 1024 * 1024), "{read} lines of 1 MiB were taken");
    }

    #[test]
    fn responses_with_literals_become_tokens() {
        let mut tokens = Vec::new();
        let literal = tokenize(
            b"* 3 FETCH (UID 17 FLAGS (\\Seen $Label1) INTERNALDATE \"17-Sep-2026 10:00:00 +0200\" BODY[] {5}\r\n",
            &mut tokens,
            usize::MAX,
        )
        .unwrap();
        assert_eq!(literal, Some(5));
        tokens.push(Token::String(b"Hallo".to_vec()));
        tokenize(b")\r\n", &mut tokens, usize::MAX).unwrap();
        let fetched = parse_fetch(Response { tokens, text: String::new() }).unwrap();
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

    /// A folder named with a trailing space ("Kunden ") is stored trimmed; its subfolders and the
    /// next round must find it again instead of making it a second time.
    #[tokio::test]
    async fn folders_with_spaces_around_their_name_are_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id) = store_with_person(dir.path(), None).await;
        let folder = |raw: &str, path: &[&str]| Folder {
            raw: raw.into(),
            path: path.iter().map(|part| part.to_string()).collect(),
            role: None,
        };
        let parent = mailbox_for(&store, id, &folder("Kunden ", &["Kunden "])).await.unwrap();
        let child = mailbox_for(&store, id, &folder("Kunden /Alt", &["Kunden ", "Alt"])).await.unwrap();
        let other = mailbox_for(&store, id, &folder("Kunden / Neu ", &["Kunden ", " Neu "])).await.unwrap();
        assert_eq!(mailbox_for(&store, id, &folder("Kunden ", &["Kunden "])).await.unwrap(), parent);
        assert_eq!(mailbox_for(&store, id, &folder("Kunden/Neu", &["Kunden", "Neu"])).await.unwrap(), other);
        let mailboxes = store.mailboxes(id).await.unwrap();
        let named = |id: i64| mailboxes.iter().find(|mailbox| mailbox.id == id).unwrap();
        assert_eq!((named(parent).name.as_str(), named(parent).parent_id), ("Kunden", None));
        assert_eq!((named(child).name.as_str(), named(child).parent_id), ("Alt", Some(parent)));
        assert_eq!((named(other).name.as_str(), named(other).parent_id), ("Neu", Some(parent)));
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

    /// A certificate for `imap.example.org` and the roots that trust it.
    fn test_tls() -> (rustls::ServerConfig, rustls::RootCertStore) {
        let generated = rcgen::generate_simple_self_signed(vec!["imap.example.org".to_owned()]).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![generated.cert.der().clone()], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(generated.cert.der().clone()).unwrap();
        (tls, roots)
    }

    fn source_at(address: String, roots: rustls::RootCertStore, password: &str) -> Source {
        Source {
            address,
            tls_name: Some("imap.example.org".into()),
            roots: Some(roots),
            master_user: None,
            password: password.into(),
            dialer: None,
        }
    }

    /// A provider that answers every command with what `answer` gives for it, then `OK`.
    pub(crate) async fn fake_provider(answer: fn(&str) -> Vec<u8>) -> Source {
        let (tls, roots) = test_tls();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = tokio_rustls::TlsAcceptor::from(Arc::new(tls)).accept(tcp).await.unwrap();
            let mut stream = BufReader::new(tls);
            stream.write_all(b"* OK fake\r\n").await.unwrap();
            stream.flush().await.unwrap();
            let mut line = String::new();
            while stream.read_line(&mut line).await.unwrap_or(0) > 0 {
                let Some((tag, command)) = line.trim_end().split_once(' ') else { break };
                let mut out = answer(command);
                out.extend(format!("{tag} OK done\r\n").bytes());
                if stream.write_all(&out).await.is_err() || stream.flush().await.is_err() {
                    break;
                }
                line.clear();
            }
        });
        source_at(address, roots, "")
    }

    /// Security review 0.22 M-1: sizes come first; small messages are fetched together, large
    /// ones alone, ones the answer leaves out alone at the limit, and ones above it not at all.
    #[tokio::test]
    async fn fetching_is_planned_by_size() {
        let source = fake_provider(|command| {
            assert_eq!(command, "UID FETCH 1,2,3,4,5,6 (UID RFC822.SIZE)");
            b"* 1 FETCH (UID 1 RFC822.SIZE 100)\r\n* 2 FETCH (UID 2 RFC822.SIZE 100)\r\n\
              * 3 FETCH (UID 3 RFC822.SIZE 20971520)\r\n* 4 FETCH (UID 4 RFC822.SIZE 100)\r\n\
              * 6 FETCH (UID 6 RFC822.SIZE 209715200)\r\n"
                .to_vec()
        })
        .await;
        let mut connection = Connection::open(&source).await.unwrap();
        let max = 50 * 1024 * 1024;
        let plan = plan_fetch(&mut connection, &[1, 2, 3, 4, 5, 6], max).await.unwrap();
        assert_eq!(plan.chunks, [vec![(1, 100), (2, 100)], vec![(3, 20_971_520)], vec![(4, 100)], vec![(5, max)]]);
        assert_eq!(plan.too_large, [6]);
    }

    /// A message of about `size` bytes with this subject.
    fn message_of(subject: &str, size: usize) -> Vec<u8> {
        let mut raw = format!(
            "From: shop@shop.example\r\nTo: mini@freemail.example\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@shop.example>\r\n\r\n"
        )
        .into_bytes();
        while raw.len() < size {
            raw.extend_from_slice(&[b'x'; 78]);
            raw.extend_from_slice(b"\r\n");
        }
        raw
    }

    /// The bytes a 2 MiB message takes that its provider says takes 100 (as Exchange's estimates
    /// can be off), UID 2 of three in INBOX.
    pub(crate) const UNDERSTATED: usize = 2 * 1024 * 1024;

    /// A provider whose `RFC822.SIZE` for UID 2 is far too small: UIDs 1 and 3 are small
    /// messages, UID 2 is [`UNDERSTATED`] bytes said to be 100.
    pub(crate) fn understating_provider(command: &str) -> Vec<u8> {
        let fetched = |uid: u32| {
            let raw = match uid {
                2 => message_of("Gross", UNDERSTATED),
                _ => message_of(&format!("Klein{uid}"), 200),
            };
            let mut out = format!(
                "* {uid} FETCH (UID {uid} FLAGS () INTERNALDATE \"17-Sep-2026 10:00:00 +0200\" BODY[] {{{}}}\r\n",
                raw.len()
            )
            .into_bytes();
            out.extend(raw);
            out.extend(b")\r\n");
            out
        };
        if command.starts_with("UID SEARCH") {
            b"* SEARCH 1 2 3\r\n".to_vec()
        } else if command.contains("RFC822.SIZE") {
            let small = message_of("Klein1", 200).len();
            format!(
                "* 1 FETCH (UID 1 RFC822.SIZE {small})\r\n* 2 FETCH (UID 2 RFC822.SIZE 100)\r\n\
                 * 3 FETCH (UID 3 RFC822.SIZE {small})\r\n"
            )
            .into_bytes()
        } else if let Some(set) =
            command.strip_prefix("UID FETCH ").and_then(|rest| rest.split_once(' ')).map(|(set, _)| set)
        {
            set.split(',').filter_map(|uid| uid.parse().ok()).flat_map(fetched).collect()
        } else {
            Vec::new()
        }
    }

    /// What a [`FetchQueue`] brings for `chunks`: each message as (UID, body length, dropped), how
    /// many portions it took, and the most body bytes one portion held.
    async fn fetch_all(
        connection: &mut Connection,
        chunks: Vec<Vec<(u32, usize)>>,
        max_size: usize,
    ) -> (Vec<(u32, Option<usize>, bool)>, usize, usize) {
        let mut queue = FetchQueue::new(chunks, max_size);
        let (mut shape, mut portions, mut most) = (Vec::new(), 0, 0);
        while let Some(portion) = queue.next(connection).await.unwrap() {
            portions += 1;
            let held: usize = portion.messages.iter().filter_map(|f| f.body.as_ref().map(Vec::len)).sum();
            most = most.max(held);
            shape.extend(portion.messages.iter().map(|f| (f.uid, f.body.as_ref().map(Vec::len), f.dropped)));
        }
        (shape, portions, most)
    }

    /// Security review 0.22 M-1 and MFIX-M1: a fetch may only take a little more than the
    /// provider said the messages take. A body larger than that is not kept but read past, and
    /// fetched again on its own with room for the largest message this server takes; one larger
    /// still comes back dropped. The others come either way, and the connection stays in step.
    #[tokio::test]
    async fn an_understated_message_is_fetched_alone_or_dropped() {
        let source = fake_provider(understating_provider).await;
        let mut connection = Connection::open(&source).await.unwrap();
        let small = message_of("Klein1", 200).len();
        let chunk = vec![(1, small), (2, 100), (3, small)];

        let (shape, ..) = fetch_all(&mut connection, vec![chunk.clone()], 100_000).await;
        assert_eq!(shape, [(1, Some(small), false), (2, None, true), (3, Some(small), false)]);

        let (shape, ..) = fetch_all(&mut connection, vec![chunk], MAX_LITERAL).await;
        let large = message_of("Gross", UNDERSTATED).len();
        assert_eq!(shape, [(1, Some(small), false), (2, Some(large), false), (3, Some(small), false)]);
    }

    static HOSTILE_FETCHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static HOSTILE_EXTRA_ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// A provider that says every message takes 100 bytes, sends each one far larger than a
    /// portion may take, answers a fetch of one message with 500 kB, and adds UIDs nobody asked
    /// for (90, 91, and the first one twice).
    fn hostile_provider(command: &str) -> Vec<u8> {
        use std::sync::atomic::Ordering;
        let literal = |uid: &str, size: usize| {
            let mut out = format!("* 1 FETCH (UID {uid} FLAGS () BODY[] {{{size}}}\r\n").into_bytes();
            out.resize(out.len() + size, b'x');
            out.extend(b")\r\n");
            out
        };
        let Some(set) =
            command.strip_prefix("UID FETCH ").and_then(|rest| rest.split_once(" (UID FLAGS")).map(|(set, _)| set)
        else {
            return Vec::new();
        };
        HOSTILE_FETCHES.fetch_add(1, Ordering::SeqCst);
        if set.split(',').any(|uid| uid == "90" || uid == "91") {
            HOSTILE_EXTRA_ASKED.store(true, Ordering::SeqCst);
        }
        if !set.contains(',') {
            return literal(set, 500_000);
        }
        let mut out: Vec<u8> = set.split(',').flat_map(|uid| literal(uid, 2_900_000)).collect();
        for extra in ["90", "91", set.split(',').next().unwrap_or("1")] {
            out.extend(literal(extra, 5));
        }
        out
    }

    /// Security review 0.22 MFIX2-H1: a provider that understates every message and answers each
    /// fetch of one with more than this server takes cannot make one import hold more than a
    /// portion's room: what is read past is fetched again one at a time, what is too large is let
    /// go at once, UIDs nobody asked for are left out and never fetched again.
    #[tokio::test]
    async fn a_hostile_provider_cannot_make_an_import_hold_more_than_its_room() {
        use std::sync::atomic::Ordering;
        let source = fake_provider(hostile_provider).await;
        let mut connection = Connection::open(&source).await.unwrap();
        let chunk: Vec<(u32, usize)> = (1..=25).map(|uid| (uid, 100)).collect();
        let (shape, portions, most) = fetch_all(&mut connection, vec![chunk], 100_000).await;
        let expected: Vec<_> = (1..=25).map(|uid| (uid, None, true)).collect();
        assert_eq!(shape, expected, "every message once, in order, dropped");
        assert_eq!(most, 0, "nothing larger than this server takes is held");
        // One portion for all, then each message read past once more on its own.
        assert_eq!((portions, HOSTILE_FETCHES.load(Ordering::SeqCst)), (26, 26));
        assert!(!HOSTILE_EXTRA_ASKED.load(Ordering::SeqCst), "UIDs nobody asked for are not fetched");
    }

    #[test]
    fn a_permit_is_never_smaller_than_asked() {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert!(runtime.block_on(import_permit(IMPORT_BUDGET_KIB * 1024 + 1)).is_err());
    }

    /// Security review 0.22 M-1: a message larger than this server takes is left out and counted
    /// as skipped; the rest comes.
    #[tokio::test(flavor = "multi_thread")]
    async fn messages_larger_than_this_server_takes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (old, old_id) = store_with_person(&dir.path().join("old"), Some("katzenpfote-123")).await;
        deliver(&old, old_id, MailboxTarget::Role(MailboxRole::Inbox), "Klein", &[]).await;
        let big =
            format!("From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: Gross\r\n\r\n{}\r\n", "x".repeat(4000));
        let request = IngestRequest {
            account_id: old_id,
            raw: big.into_bytes(),
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: Vec::new(),
            received_at: Some(1_700_000_000),
        };
        old.ingest(request).await.unwrap();

        let (tls, roots) = test_tls();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(uwumail_imap::Imap::new(old.clone(), 1 << 20).serve(listener, Arc::new(tls), shutdown_rx));
        let source = source_at(address, roots, "katzenpfote-123");

        let (new, new_id) = store_with_person(&dir.path().join("new"), None).await;
        let mut connection = Connection::open(&source).await.unwrap();
        connection.command("LOGIN mini@example.org katzenpfote-123").await.unwrap();
        let mut report =
            |_: CopyEvent| -> Pin<Box<dyn Future<Output = bool> + Send>> { Box::pin(std::future::ready(true)) };
        let options = CopyOptions { max_size: 2000, ..CopyOptions::default() };
        let (copied, _) = copy_folders(&new, &mut connection, new_id, "test", options, &mut report).await.unwrap();
        assert_eq!((copied.messages, copied.skipped), (1, 1));
        let inbox = new.mailboxes(new_id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
        let emails = new.emails_in_mailbox(inbox.unwrap().id, 10).await.unwrap();
        assert_eq!(emails.iter().map(|email| email.subject.as_str()).collect::<Vec<_>>(), ["Klein"]);
    }

    #[test]
    fn only_messages_that_are_nothing_but_an_object_count_as_one() {
        let mail =
            |body: &str| format!("From: a@example.org\r\nSubject: x\r\nMIME-Version: 1.0\r\n{body}").into_bytes();
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:Kim\r\nEND:VCARD\r\n";
        let event = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        // A bare object, and Kolab's: a short note next to it.
        let bare = mail(&format!("Content-Type: text/calendar\r\n\r\n{event}"));
        assert_eq!(pure_object_texts(&bare, DavKind::Calendar).len(), 1);
        assert!(pure_object_texts(&bare, DavKind::Addressbook).is_empty(), "only the kind asked for");
        let kolab = mail(&format!(
            "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nThis is a Kolab Groupware object.\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b\r\nContent-Type: application/x-vnd.kolab.contact\r\n\r\n<contact/>\r\n--b--\r\n"
        ));
        assert_eq!(pure_object_texts(&kolab, DavKind::Addressbook).len(), 1);
        // Letters are mail, whatever they carry: an HTML body, a long text, another attachment.
        for body in [
            format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Hi</p>\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b--\r\n"
            ),
            format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n{}\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b--\r\n",
                "Lange Nachricht. ".repeat(40)
            ),
            format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b\r\nContent-Type: application/pdf\r\n\r\nJVBERi0x\r\n--b--\r\n"
            ),
            format!(
                "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=a.txt\r\n\r\nx\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b--\r\n"
            ),
            "Content-Type: text/plain\r\n\r\nNur Text\r\n".to_owned(),
        ] {
            assert!(pure_object_texts(&mail(&body), DavKind::Addressbook).is_empty(), "{body}");
        }
        // An invitation with a short note is mail, not a stored event.
        let invitation = mail(
            "Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nKommst du?\r\n--b\r\nContent-Type: text/calendar\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--b--\r\n",
        );
        assert!(pure_object_texts(&invitation, DavKind::Calendar).is_empty());
        // The header alone does not lift the limit on the note.
        let forged = format!(
            "X-Kolab-Type: application/x-vnd.kolab.contact\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n{}\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b--\r\n",
            "Lange Nachricht. ".repeat(40)
        );
        assert!(pure_object_texts(&mail(&forged), DavKind::Addressbook).is_empty());
        // With Kolab's header and format the note may be longer.
        let long_note = format!(
            "X-Kolab-Type: application/x-vnd.kolab.contact\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n{}\r\n--b\r\nContent-Type: text/vcard\r\n\r\n{card}--b\r\nContent-Type: application/x-vnd.kolab.contact\r\n\r\n<contact/>\r\n--b--\r\n",
            "Hinweis. ".repeat(80)
        );
        assert_eq!(pure_object_texts(&mail(&long_note), DavKind::Addressbook).len(), 1);
    }

    #[test]
    fn folders_find_their_role() {
        let path = |name: &str| vec![name.to_owned()];
        assert_eq!(role_of(&["\\HasNoChildren".into(), "\\Sent".into()], &path("Gesendet")), Some(MailboxRole::Sent));
        assert_eq!(role_of(&[], &path("Papierkorb")), Some(MailboxRole::Trash));
        assert_eq!(role_of(&[], &["Archiv".into(), "2025".into()]), None);
        assert_eq!(quoted("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    /// A name a person typed that also leads to this machine: nothing from the fetch or the move
    /// may ever arrive here, whether the admin routes fetching through a proxy or not.
    #[tokio::test]
    async fn a_person_s_provider_never_leads_to_this_machine() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let egress = uwumail_smtp::egress::Egress::direct();
        for dialer in [Some(egress.dialer(uwumail_smtp::egress::Purpose::Fetch)), None] {
            let source = Source::remote("localhost", port, "katzenpfote-123".into(), dialer);
            // Refused straight away; a connection that got through would wait for a TLS answer.
            let opened = tokio::time::timeout(Duration::from_secs(10), Connection::open(&source)).await;
            assert!(matches!(opened, Ok(Err(_))), "the provider's name was not refused");
            let knocked = listener.accept();
            assert!(
                matches!(&knocked, Err(err) if err.kind() == std::io::ErrorKind::WouldBlock),
                "a connection reached this machine: {knocked:?}"
            );
        }
        assert!(!resolves_publicly("localhost", port).await);
    }
}
