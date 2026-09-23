//! ManageSieve (RFC 5804): mail apps and tools manage the account's Sieve scripts, the same ones
//! JMAP shows as `SieveScript` (docs/sieve.md).
//!
//! The connection starts in plain text and has to switch to TLS with STARTTLS before AUTHENTICATE
//! PLAIN is offered at all. Logins go through the same checks, app passwords and lockouts as IMAP:
//! the limiter is shared, so a network that guesses passwords here is shut out there too.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio_rustls::TlsAcceptor;
use uwumail_smtp::{AuthLimiter, BoxIo};
use uwumail_store::{
    Account, AppScope, MailAuth, MailAuthDenied, SIEVE_MAX_SCRIPT_SIZE, SIEVE_MAX_SCRIPTS, SieveError, Store,
    validate_sieve_name,
};

use crate::Imap;

/// The longest command line, without literals.
const MAX_LINE: usize = 8 * 1024;
/// The largest literal before logging in: nothing but AUTHENTICATE data needs one.
const MAX_LITERAL_BEFORE_LOGIN: usize = 4 * 1024;
/// A literal this much over the script limit is read and refused; beyond, the connection ends.
const MAX_LITERAL_DISCARD: usize = 1024 * 1024;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
/// RFC 5804 section 1.2: at least 30 minutes once logged in.
const IDLE_TIMEOUT: Duration = Duration::from_secs(31 * 60);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_AUTH_FAILURES: u32 = 3;
const MAX_CONNECTIONS: usize = 500;
const IMPLEMENTATION: &str = "UwUMail Server";

/// Everything ManageSieve connections share. Cheap to clone.
#[derive(Clone)]
pub struct ManageSieve {
    store: Store,
    limiter: Arc<AuthLimiter>,
    connections: Arc<Semaphore>,
}

impl ManageSieve {
    /// Shares the store and the login limiter with IMAP.
    pub fn new(imap: &Imap) -> ManageSieve {
        ManageSieve {
            store: imap.store.clone(),
            limiter: imap.limiter.clone(),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    /// Accepts connections (port 4190) until `shutdown` changes.
    pub async fn serve(
        self,
        listener: TcpListener,
        tls: Arc<rustls::ServerConfig>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((socket, peer)) => {
                        let _ = socket.set_nodelay(true);
                        self.serve_stream(Box::new(socket), peer, tls.clone());
                    }
                    Err(err) => {
                        tracing::warn!(%err, "accepting a managesieve connection failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                _ = shutdown.changed() => break,
            }
        }
    }

    /// Runs one session on a connection from `peer`.
    pub fn serve_stream(&self, stream: BoxIo, peer: SocketAddr, tls: Arc<rustls::ServerConfig>) {
        let sieve = self.clone();
        tokio::spawn(async move {
            let Ok(_permit) = sieve.connections.clone().try_acquire_owned() else {
                return;
            };
            let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
            let mut session = Session {
                sieve,
                peer,
                stream: Some(BufReader::new(stream)),
                tls,
                encrypted: false,
                account: None,
                auth_failures: 0,
            };
            match session.run().await {
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
                    ) => {}
                Err(err) => tracing::debug!(%peer, %err, "managesieve session ended with an error"),
                Ok(()) => {}
            }
        });
    }
}

/// A string as ManageSieve sends it: quoted when it can be, a literal otherwise.
fn string(value: &str) -> String {
    if value.len() <= 1024 && !value.contains(['\r', '\n', '\0']) {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("{{{}}}\r\n{value}", value.len())
    }
}

fn ok(text: &str) -> String {
    format!("OK {}\r\n", string(text))
}

fn no(code: Option<&str>, text: &str) -> String {
    match code {
        Some(code) => format!("NO ({code}) {}\r\n", string(text)),
        None => format!("NO {}\r\n", string(text)),
    }
}

/// An argument of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Arg {
    Atom(String),
    Text(Vec<u8>),
}

impl Arg {
    fn text(&self) -> Option<&[u8]> {
        match self {
            Arg::Text(bytes) => Some(bytes),
            Arg::Atom(_) => None,
        }
    }

    fn utf8(&self) -> Option<&str> {
        self.text().and_then(|bytes| std::str::from_utf8(bytes).ok())
    }
}

/// What reading a command came to.
enum Read {
    Command(Vec<Arg>),
    /// A literal bigger than allowed was read and dropped.
    TooBig,
    Closed,
}

enum Flow {
    Continue,
    Close,
}

/// Parses the tokens of one line. A literal announced at the end comes back as its size.
fn tokenize(line: &[u8], args: &mut Vec<Arg>) -> Result<Option<usize>, &'static str> {
    let mut pos = 0;
    while pos < line.len() {
        match line[pos] {
            b' ' | b'\t' => pos += 1,
            b'"' => {
                let mut value = Vec::new();
                pos += 1;
                loop {
                    match line.get(pos) {
                        None => return Err("Unterminated string"),
                        Some(b'"') => break,
                        Some(b'\\') => {
                            value.push(*line.get(pos + 1).ok_or("Unterminated string")?);
                            pos += 2;
                        }
                        Some(&byte) => {
                            value.push(byte);
                            pos += 1;
                        }
                    }
                }
                pos += 1;
                args.push(Arg::Text(value));
            }
            b'{' => {
                let end = line[pos..].iter().position(|&b| b == b'}').ok_or("Bad literal")? + pos;
                if end + 1 != line.len() {
                    return Err("A literal must end the line");
                }
                let inner = &line[pos + 1..end];
                let digits = inner.strip_suffix(b"+").unwrap_or(inner);
                if digits.is_empty() || digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
                    return Err("Bad literal");
                }
                let size: usize = std::str::from_utf8(digits).ok().and_then(|d| d.parse().ok()).ok_or("Bad literal")?;
                return Ok(Some(size));
            }
            _ => {
                let start = pos;
                while pos < line.len() && !matches!(line[pos], b' ' | b'\t' | b'"' | b'{' | b'(' | b')') {
                    pos += 1;
                }
                if pos == start {
                    return Err("Unexpected character");
                }
                args.push(Arg::Atom(String::from_utf8_lossy(&line[start..pos]).into_owned()));
            }
        }
    }
    Ok(None)
}

struct Session {
    sieve: ManageSieve,
    peer: SocketAddr,
    /// `None` only while TLS is being set up.
    stream: Option<BufReader<BoxIo>>,
    tls: Arc<rustls::ServerConfig>,
    encrypted: bool,
    account: Option<Account>,
    auth_failures: u32,
}

impl Session {
    fn stream(&mut self) -> &mut BufReader<BoxIo> {
        self.stream.as_mut().expect("the stream is back after TLS")
    }

    async fn send(&mut self, text: &str) -> io::Result<()> {
        let stream = self.stream().get_mut();
        stream.write_all(text.as_bytes()).await?;
        stream.flush().await
    }

    fn capabilities(&self) -> String {
        let mut lines = vec![
            format!("\"IMPLEMENTATION\" {}", string(IMPLEMENTATION)),
            format!("\"SIEVE\" {}", string(&uwumail_smtp::sieve::EXTENSIONS.join(" "))),
            format!("\"SASL\" {}", string(if self.encrypted { "PLAIN" } else { "" })),
            format!("\"MAXREDIRECTS\" \"{}\"", uwumail_smtp::sieve::MAX_REDIRECTS),
            "\"VERSION\" \"1.0\"".to_owned(),
            "\"UNAUTHENTICATE\"".to_owned(),
        ];
        if !self.encrypted {
            lines.push("\"STARTTLS\"".to_owned());
        }
        if let Some(account) = &self.account {
            lines.push(format!("\"OWNER\" {}", string(&account.login)));
        }
        let mut text = lines.join("\r\n");
        text.push_str("\r\n");
        text
    }

    async fn run(&mut self) -> io::Result<()> {
        let greeting = format!("{}OK {}\r\n", self.capabilities(), string("UwUMail ManageSieve ready, nya"));
        self.send(&greeting).await?;
        loop {
            let limit = if self.account.is_some() { IDLE_TIMEOUT } else { LOGIN_TIMEOUT };
            let read = match tokio::time::timeout(limit, self.read_command()).await {
                Ok(Ok(read)) => read,
                Ok(Err(err)) if err.kind() == io::ErrorKind::InvalidData => {
                    self.send(&format!("BYE {}\r\n", string(&err.to_string()))).await?;
                    return Ok(());
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    self.send(&format!("BYE {}\r\n", string("You were idle for too long, bye"))).await?;
                    return Ok(());
                }
            };
            let args = match read {
                Read::Command(args) => args,
                Read::TooBig => {
                    self.send(&no(Some("QUOTA/MAXSIZE"), "That is more than a script may have")).await?;
                    continue;
                }
                Read::Closed => return Ok(()),
            };
            if let Flow::Close = self.dispatch(args).await? {
                return Ok(());
            }
        }
    }

    async fn read_line(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        let read = (&mut *self.stream()).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line).await?;
        if read > MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Line too long"));
        }
        if !line.ends_with(b"\n") {
            return Ok(Vec::new());
        }
        line.pop();
        if line.ends_with(b"\r") {
            line.pop();
        }
        Ok(line)
    }

    /// Reads a command with its literals.
    async fn read_command(&mut self) -> io::Result<Read> {
        let mut args = Vec::new();
        let mut too_big = false;
        let mut total = 0usize;
        loop {
            let line = self.read_line().await?;
            if line.is_empty() && args.is_empty() {
                // A client that hung up, or an empty line, which says nothing.
                if self.stream().buffer().is_empty() && self.stream().fill_buf().await?.is_empty() {
                    return Ok(Read::Closed);
                }
                continue;
            }
            let size = tokenize(&line, &mut args).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            let Some(size) = size else {
                return Ok(if too_big { Read::TooBig } else { Read::Command(args) });
            };
            let limit = if self.account.is_some() { SIEVE_MAX_SCRIPT_SIZE } else { MAX_LITERAL_BEFORE_LOGIN };
            total += size;
            if size > limit {
                if self.account.is_none() || total > MAX_LITERAL_DISCARD {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Literal too big"));
                }
                // Read what the client sends anyway, and answer once the command is over.
                tokio::io::copy(&mut (&mut *self.stream()).take(size as u64), &mut tokio::io::sink()).await?;
                too_big = true;
                args.push(Arg::Text(Vec::new()));
                continue;
            }
            let mut literal = vec![0; size];
            self.stream().read_exact(&mut literal).await?;
            args.push(Arg::Text(literal));
        }
    }

    async fn dispatch(&mut self, args: Vec<Arg>) -> io::Result<Flow> {
        let Some(Arg::Atom(command)) = args.first() else {
            self.send(&no(None, "Expected a command")).await?;
            return Ok(Flow::Continue);
        };
        let command = command.to_ascii_uppercase();
        let params = &args[1..];
        let logged_in = self.account.is_some();
        let answer = match (command.as_str(), logged_in) {
            ("CAPABILITY", _) => format!("{}OK {}\r\n", self.capabilities(), string("Capability completed")),
            ("NOOP", _) => match params.first().and_then(Arg::text) {
                Some(tag) => format!("OK (TAG {}) {}\r\n", string(&String::from_utf8_lossy(tag)), string("Done")),
                None => ok("NOOP completed"),
            },
            ("LOGOUT", _) => {
                self.send(&ok("Logout completed, see you")).await?;
                return Ok(Flow::Close);
            }
            ("STARTTLS", false) => return self.starttls().await,
            ("AUTHENTICATE", false) => return self.authenticate(params).await,
            ("STARTTLS" | "AUTHENTICATE", true) => no(None, "Already logged in"),
            ("UNAUTHENTICATE", true) => {
                self.account = None;
                ok("Logged out, the connection stays")
            }
            ("UNAUTHENTICATE", false) => no(None, "Not logged in"),
            (
                "HAVESPACE" | "PUTSCRIPT" | "LISTSCRIPTS" | "SETACTIVE" | "GETSCRIPT" | "DELETESCRIPT" | "RENAMESCRIPT"
                | "CHECKSCRIPT",
                false,
            ) => no(None, "Log in first"),
            ("HAVESPACE", true) => self.scripts().havespace(params).await,
            ("PUTSCRIPT", true) => self.scripts().putscript(params).await,
            ("LISTSCRIPTS", true) => self.scripts().listscripts().await,
            ("SETACTIVE", true) => self.scripts().setactive(params).await,
            ("GETSCRIPT", true) => self.scripts().getscript(params).await,
            ("DELETESCRIPT", true) => self.scripts().deletescript(params).await,
            ("RENAMESCRIPT", true) => self.scripts().renamescript(params).await,
            ("CHECKSCRIPT", true) => match params.first().and_then(Arg::text) {
                Some(script) => match uwumail_smtp::sieve::validate(script) {
                    Ok(()) => ok("The script is fine"),
                    Err(problem) => no(None, &problem),
                },
                None => no(None, "Expected the script"),
            },
            _ => no(None, "Unknown command"),
        };
        self.send(&answer).await?;
        Ok(Flow::Continue)
    }

    async fn starttls(&mut self) -> io::Result<Flow> {
        if self.encrypted {
            self.send(&no(None, "TLS is already on")).await?;
            return Ok(Flow::Continue);
        }
        self.send(&ok("Begin TLS negotiation now")).await?;
        let stream = self.stream.take().expect("stream");
        // Anything sent behind STARTTLS before the handshake would be read as if it were encrypted.
        if !stream.buffer().is_empty() {
            return Ok(Flow::Close);
        }
        let acceptor = TlsAcceptor::from(self.tls.clone());
        let tls = match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream.into_inner())).await {
            Ok(Ok(tls)) => tls,
            Ok(Err(err)) => {
                tracing::debug!(peer = %self.peer, %err, "managesieve tls handshake failed");
                return Ok(Flow::Close);
            }
            Err(_) => return Ok(Flow::Close),
        };
        self.stream = Some(BufReader::new(Box::new(tls) as BoxIo));
        self.encrypted = true;
        // RFC 5804 section 2.2: the capabilities again, now with SASL mechanisms.
        let capabilities = format!("{}OK {}\r\n", self.capabilities(), string("TLS is on, you may log in"));
        self.send(&capabilities).await?;
        Ok(Flow::Continue)
    }

    async fn authenticate(&mut self, params: &[Arg]) -> io::Result<Flow> {
        let mechanism = params.first().and_then(Arg::utf8).unwrap_or_default();
        if !mechanism.eq_ignore_ascii_case("PLAIN") {
            self.send(&no(None, "Only PLAIN is supported")).await?;
            return Ok(Flow::Continue);
        }
        if !self.encrypted {
            self.send(&no(Some("ENCRYPT-NEEDED"), "Switch to TLS with STARTTLS first")).await?;
            return Ok(Flow::Continue);
        }
        let response = match params.get(1) {
            Some(initial) => initial.text().map(<[u8]>::to_vec),
            None => {
                self.send("\"\"\r\n").await?;
                match self.read_command().await? {
                    Read::Command(args) => args.first().and_then(Arg::text).map(<[u8]>::to_vec),
                    Read::TooBig | Read::Closed => None,
                }
            }
        };
        let Some(response) = response.filter(|response| response != b"*") else {
            self.send(&no(None, "Authentication cancelled")).await?;
            return Ok(Flow::Continue);
        };
        let decoded = base64::engine::general_purpose::STANDARD.decode(response.trim_ascii()).ok();
        let parts: Option<Vec<String>> = decoded.and_then(|bytes| {
            let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
            (parts.len() == 3).then(|| parts.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect())
        });
        let Some(parts) = parts else {
            self.send(&no(None, "Invalid PLAIN data")).await?;
            return Ok(Flow::Continue);
        };
        if !parts[0].is_empty() && !parts[0].eq_ignore_ascii_case(&parts[1]) {
            self.send(&no(None, "Logging in as someone else is not allowed")).await?;
            return Ok(Flow::Continue);
        }
        self.login(&parts[1], &parts[2]).await
    }

    async fn login(&mut self, username: &str, password: &str) -> io::Result<Flow> {
        let limiter = self.sieve.limiter.clone();
        if limiter.is_blocked(self.peer.ip()) {
            self.send(&no(Some("TRYLATER"), "Too many failed logins, try again later")).await?;
            return Ok(Flow::Continue);
        }
        let peer = self.peer.to_string();
        match self.sieve.store.authenticate_mail(username, password, AppScope::Mail, "managesieve", &peer).await {
            Ok(MailAuth::Ok { account, app_password }) => {
                limiter.record_success(self.peer.ip());
                tracing::info!(login = %account.login, peer = %self.peer, app_password = app_password.is_some(), "managesieve login");
                self.account = Some(account);
                self.send(&ok("Logged in, hi")).await?;
                Ok(Flow::Continue)
            }
            Ok(MailAuth::Denied(reason)) => {
                match reason {
                    MailAuthDenied::AppPasswordRequired => {}
                    MailAuthDenied::UnknownLogin => limiter.record_unknown_login(self.peer.ip()),
                    _ => limiter.record_failure(self.peer.ip()),
                }
                self.auth_failures += 1;
                tracing::warn!(login = %username, peer = %self.peer, %reason, "failed managesieve login");
                tokio::time::sleep(Duration::from_secs(1)).await;
                let text = match reason {
                    MailAuthDenied::AppPasswordRequired => "This account needs an app password for mail apps",
                    _ => "Wrong login or password",
                };
                if self.auth_failures >= MAX_AUTH_FAILURES {
                    self.send(&format!("BYE {}\r\n", string("Too many failed logins"))).await?;
                    return Ok(Flow::Close);
                }
                self.send(&no(None, text)).await?;
                Ok(Flow::Continue)
            }
            Err(err) => {
                tracing::error!(%err, "managesieve authentication failed internally");
                self.send(&no(Some("TRYLATER"), "Temporary authentication failure")).await?;
                Ok(Flow::Continue)
            }
        }
    }

    /// What the script commands work on: the logged-in account's scripts.
    fn scripts(&self) -> Scripts {
        Scripts { store: self.sieve.store.clone(), account: self.account.as_ref().map_or(0, |account| account.id) }
    }
}

/// The script commands, for one account. Apart from the session, whose connection may not be
/// shared between threads while a command waits for the store.
struct Scripts {
    store: Store,
    account: i64,
}

impl Scripts {
    fn store_error(err: SieveError) -> String {
        match err {
            SieveError::AlreadyExists(_) => no(Some("ALREADYEXISTS"), "A script with that name exists already"),
            SieveError::TooMany => no(Some("QUOTA/MAXSCRIPTS"), &format!("At most {SIEVE_MAX_SCRIPTS} scripts")),
            SieveError::TooLarge => no(Some("QUOTA/MAXSIZE"), "That is more than a script may have"),
            SieveError::InvalidName(problem) | SieveError::InvalidContent(problem) => no(None, &problem),
            SieveError::NotFound => no(Some("NONEXISTENT"), "There is no script by that name"),
            SieveError::Active => no(Some("ACTIVE"), "Switch the script off before deleting it"),
            SieveError::Store(err) => {
                tracing::error!(%err, "a managesieve command failed in the store");
                no(Some("TRYLATER"), "Something went wrong, please try again")
            }
        }
    }

    /// The script name argument, checked like the store checks it.
    fn name(params: &[Arg], index: usize) -> Result<String, String> {
        let name = params.get(index).and_then(Arg::utf8).ok_or_else(|| no(None, "Expected a script name"))?;
        validate_sieve_name(name).map_err(|err| no(None, &err.to_string()))?;
        Ok(name.to_owned())
    }

    async fn id_named(&self, name: &str) -> Result<Option<i64>, String> {
        match self.store.sieve_script_named(self.account, name).await {
            Ok(found) => Ok(found.map(|(script, _)| script.id)),
            Err(err) => Err(Self::store_error(err.into())),
        }
    }

    async fn havespace(&self, params: &[Arg]) -> String {
        let name = match Self::name(params, 0) {
            Ok(name) => name,
            Err(answer) => return answer,
        };
        let Some(size) = params.get(1).and_then(|arg| match arg {
            Arg::Atom(number) => number.parse::<u64>().ok(),
            Arg::Text(_) => None,
        }) else {
            return no(None, "Expected a size");
        };
        if size > SIEVE_MAX_SCRIPT_SIZE as u64 {
            return no(Some("QUOTA/MAXSIZE"), "That is more than a script may have");
        }
        match self.id_named(&name).await {
            Ok(Some(_)) => ok("There is room"),
            Ok(None) => match self.store.sieve_scripts(self.account).await {
                Ok(scripts) if scripts.len() >= SIEVE_MAX_SCRIPTS => {
                    no(Some("QUOTA/MAXSCRIPTS"), &format!("At most {SIEVE_MAX_SCRIPTS} scripts"))
                }
                Ok(_) => ok("There is room"),
                Err(err) => Self::store_error(err.into()),
            },
            Err(answer) => answer,
        }
    }

    async fn putscript(&self, params: &[Arg]) -> String {
        let name = match Self::name(params, 0) {
            Ok(name) => name,
            Err(answer) => return answer,
        };
        let Some(script) = params.get(1).and_then(Arg::text) else {
            return no(None, "Expected the script");
        };
        if let Err(problem) = uwumail_smtp::sieve::validate(script) {
            return no(None, &problem);
        }
        match self.store.put_sieve_script(self.account, &name, script).await {
            Ok(_) => ok("Stored"),
            Err(err) => Self::store_error(err),
        }
    }

    async fn listscripts(&self) -> String {
        match self.store.sieve_scripts(self.account).await {
            Ok(scripts) => {
                let mut answer = String::new();
                for script in scripts {
                    answer.push_str(&string(&script.name));
                    if script.is_active {
                        answer.push_str(" ACTIVE");
                    }
                    answer.push_str("\r\n");
                }
                answer.push_str(&ok("Listscripts completed"));
                answer
            }
            Err(err) => Self::store_error(err.into()),
        }
    }

    async fn setactive(&self, params: &[Arg]) -> String {
        let Some(name) = params.first().and_then(Arg::utf8) else {
            return no(None, "Expected a script name");
        };
        let id = if name.is_empty() {
            None
        } else {
            match self.id_named(name).await {
                Ok(Some(id)) => Some(id),
                Ok(None) => return no(Some("NONEXISTENT"), "There is no script by that name"),
                Err(answer) => return answer,
            }
        };
        match self.store.activate_sieve_script(self.account, id).await {
            Ok(_) => ok(if id.is_some() { "The script is active" } else { "No script is active now" }),
            Err(err) => Self::store_error(err),
        }
    }

    async fn getscript(&self, params: &[Arg]) -> String {
        let Some(name) = params.first().and_then(Arg::utf8) else {
            return no(None, "Expected a script name");
        };
        match self.store.sieve_script_named(self.account, name).await {
            Ok(Some((_, content))) => format!("{{{}}}\r\n{content}\r\n{}", content.len(), ok("Getscript completed")),
            Ok(None) => no(Some("NONEXISTENT"), "There is no script by that name"),
            Err(err) => Self::store_error(err.into()),
        }
    }

    async fn deletescript(&self, params: &[Arg]) -> String {
        let Some(name) = params.first().and_then(Arg::utf8) else {
            return no(None, "Expected a script name");
        };
        let id = match self.id_named(name).await {
            Ok(Some(id)) => id,
            Ok(None) => return no(Some("NONEXISTENT"), "There is no script by that name"),
            Err(answer) => return answer,
        };
        match self.store.destroy_sieve_script(self.account, id).await {
            Ok(()) => ok("Deleted"),
            Err(err) => Self::store_error(err),
        }
    }

    async fn renamescript(&self, params: &[Arg]) -> String {
        let Some(old) = params.first().and_then(Arg::utf8) else {
            return no(None, "Expected a script name");
        };
        let new = match Self::name(params, 1) {
            Ok(name) => name,
            Err(answer) => return answer,
        };
        let id = match self.id_named(old).await {
            Ok(Some(id)) => id,
            Ok(None) => return no(Some("NONEXISTENT"), "There is no script by that name"),
            Err(answer) => return answer,
        };
        match self.store.update_sieve_script(self.account, id, Some(&new), None).await {
            Ok(_) => ok("Renamed"),
            Err(err) => Self::store_error(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> (Vec<Arg>, Option<usize>) {
        let mut args = Vec::new();
        let size = tokenize(line.as_bytes(), &mut args).unwrap();
        (args, size)
    }

    #[test]
    fn commands_are_split_into_atoms_strings_and_literals() {
        let (args, size) = parse("Putscript \"my \\\"best\\\" script\" {31+}");
        assert_eq!(args, [Arg::Atom("Putscript".into()), Arg::Text(b"my \"best\" script".to_vec())]);
        assert_eq!(size, Some(31));
        assert_eq!(parse("HAVESPACE \"x\" 435").0[2], Arg::Atom("435".into()));
        assert_eq!(parse("checkscript {5}").1, Some(5), "a literal without + is taken too");
        let mut args = Vec::new();
        assert!(tokenize(b"x \"open", &mut args).is_err());
        assert!(tokenize(b"x {12+} more", &mut args).is_err());
        assert!(tokenize(b"x {99999999999+}", &mut args).is_err());
    }

    #[test]
    fn strings_are_quoted_unless_they_cannot_be() {
        assert_eq!(string("a \"b\" \\c"), "\"a \\\"b\\\" \\\\c\"");
        assert_eq!(string("two\r\nlines"), "{10}\r\ntwo\r\nlines");
        assert_eq!(no(Some("NONEXISTENT"), "gone"), "NO (NONEXISTENT) \"gone\"\r\n");
    }
}
