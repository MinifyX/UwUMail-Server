//! SMTP server sessions: MX (port 25) and submission (587 with STARTTLS, 465 with TLS).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use mail_builder::headers::date::Date;
use smtp_proto::request::receiver::{BdatReceiver, DataReceiver, DummyDataReceiver, LineReceiver, RequestReceiver};
use smtp_proto::{AUTH_LOGIN, AUTH_PLAIN, MailFrom, RcptTo, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use uwumail_store::{
    Account, AppScope, IngestRequest, MailAuth, MailAuthDenied, MailboxRole, MailboxTarget, NewQueueRecipient,
    ReportKind, StoreError,
};

use crate::checks::{self, Action};
use crate::dsn::{self, FailedRecipient};
use crate::sender_lists::{self, Decision};
use crate::stream::{BoxIo, Stream};
use crate::submission::{Submission, SubmissionRecipient, SubmitError};
use crate::{Smtp, forward, headers, random_id, relay, reports, spam, srs, vacation};

const MAX_HOPS: usize = 50;
const MAX_ERRORS: u32 = 10;
const MAX_AUTH_FAILURES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerKind {
    /// Port 25: mail for our domains from other servers. No authentication.
    Mx,
    /// Port 587: authenticated submission after STARTTLS.
    Submission,
    /// Port 465: authenticated submission over implicit TLS.
    SubmissionTls,
}

impl ListenerKind {
    fn is_submission(self) -> bool {
        !matches!(self, ListenerKind::Mx)
    }
}

/// Accepts connections until `shutdown` changes.
pub async fn serve(smtp: Smtp, listener: TcpListener, kind: ListenerKind, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((socket, peer)) => {
                    let _ = socket.set_nodelay(true);
                    tokio::spawn(serve_stream(smtp.clone(), Box::new(socket), peer, kind));
                }
                Err(err) => {
                    tracing::warn!(%err, "accepting an smtp connection failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = shutdown.changed() => break,
        }
    }
}

/// Runs one session on a connection from `peer`, which arrived on a listener or through the
/// UwUMail Gateway.
pub async fn serve_stream(smtp: Smtp, stream: BoxIo, peer: SocketAddr, kind: ListenerKind) {
    match handle(smtp, stream, peer, kind).await {
        // Health checks and port scanners hang up without saying goodbye.
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) => {}
        Err(err) => tracing::debug!(%peer, ?kind, %err, "smtp session ended with an error"),
        Ok(()) => {}
    }
}

async fn handle(smtp: Smtp, mut socket: BoxIo, peer: SocketAddr, kind: ListenerKind) -> std::io::Result<()> {
    let ctx = &smtp.inner;
    let Ok(_permit) = ctx.connections.clone().try_acquire_owned() else {
        let _ = socket.write_all(b"421 4.3.2 Too many connections, try again later\r\n").await;
        return Ok(());
    };
    let stream = if kind == ListenerKind::SubmissionTls {
        let Some(tls) = ctx.server_tls.clone() else {
            return Ok(());
        };
        let accepted = timeout(Duration::from_secs(30), TlsAcceptor::from(tls).accept(socket))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
        Stream::Server(Box::new(accepted))
    } else {
        Stream::Plain(socket)
    };
    // Dual-stack listeners report IPv4 clients as ::ffff:a.b.c.d; SPF needs the plain IPv4 address.
    let mut session = Session::new(smtp.clone(), stream, peer.ip().to_canonical(), kind);
    session.run().await
}

struct Envelope {
    address: String,
    size: usize,
    env_id: Option<String>,
}

struct Recipient {
    address: String,
    local_account: Option<i64>,
    /// A bounce for a forwarded message, to pass on to this original sender.
    srs_return: Option<String>,
    /// DMARC or TLS reports for one of our domains, read by the server itself.
    report: Option<ReportKind>,
    /// A forwarding address without a mailbox: where its mail goes, `Some(account id)` for people here.
    forward_to: Option<Vec<(String, Option<i64>)>>,
    notify_flags: u64,
    orcpt: Option<String>,
}

enum AuthStep {
    PlainResponse,
    LoginUsername,
    LoginPassword(String),
}

enum State {
    Command(RequestReceiver),
    Data(DataReceiver),
    DataDiscard(DummyDataReceiver),
    Bdat(BdatReceiver),
    BdatDiscard(DummyDataReceiver),
    Auth(AuthStep, LineReceiver<()>),
}

enum Next {
    Continue,
    Data,
    Bdat { size: usize, last: bool },
    BdatDiscard(usize),
    Auth(AuthStep),
    StartTls,
    Quit,
}

struct Session {
    smtp: Smtp,
    stream: Option<Stream>,
    peer: IpAddr,
    kind: ListenerKind,
    helo: Option<String>,
    account: Option<Account>,
    envelope: Option<Envelope>,
    recipients: Vec<Recipient>,
    message: Vec<u8>,
    message_too_big: bool,
    errors: u32,
    auth_failures: u32,
}

impl Session {
    fn new(smtp: Smtp, stream: Stream, peer: IpAddr, kind: ListenerKind) -> Session {
        Session {
            smtp,
            stream: Some(stream),
            peer,
            kind,
            helo: None,
            account: None,
            envelope: None,
            recipients: Vec::new(),
            message: Vec::new(),
            message_too_big: false,
            errors: 0,
            auth_failures: 0,
        }
    }

    fn stream(&mut self) -> &mut Stream {
        self.stream.as_mut().expect("the stream is only taken during STARTTLS")
    }

    fn is_tls(&self) -> bool {
        self.stream.as_ref().is_some_and(Stream::is_tls)
    }

    async fn reply(&mut self, text: &str) -> std::io::Result<()> {
        let stream = self.stream();
        stream.write_all(text.as_bytes()).await?;
        stream.flush().await
    }

    async fn run(&mut self) -> std::io::Result<()> {
        let greeting = format!("220 {} ESMTP UwUMail ready\r\n", self.smtp.inner.hostname);
        self.reply(&greeting).await?;

        let idle = Duration::from_secs(self.smtp.inner.live().smtp.timeout_secs.max(10));
        let max_size = self.smtp.inner.live().smtp.max_message_size;
        let mut buf = vec![0u8; 16 * 1024];
        let mut state = State::Command(RequestReceiver::default());

        loop {
            let read = match timeout(idle, self.stream().read(&mut buf)).await {
                Ok(Ok(0)) => return Ok(()),
                Ok(Ok(n)) => n,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    let _ = self.reply("421 4.4.2 Idle for too long, closing connection\r\n").await;
                    return Ok(());
                }
            };
            let mut bytes = buf[..read].iter();

            loop {
                match &mut state {
                    State::Command(receiver) => match receiver.ingest(&mut bytes) {
                        Ok(request) => {
                            let request = request.into_owned();
                            match self.command(request).await? {
                                Next::Continue => {}
                                Next::Data => {
                                    self.message.clear();
                                    self.message_too_big = false;
                                    state = State::Data(DataReceiver::new());
                                }
                                Next::Bdat { size, last } => state = State::Bdat(BdatReceiver::new(size, last)),
                                Next::BdatDiscard(size) => {
                                    state = State::BdatDiscard(DummyDataReceiver::new_bdat(size))
                                }
                                Next::Auth(step) => state = State::Auth(step, LineReceiver::new(())),
                                Next::StartTls => {
                                    self.start_tls().await?;
                                    // Anything the client pipelined before the handshake is discarded (RFC 3207).
                                    state = State::Command(RequestReceiver::default());
                                    break;
                                }
                                Next::Quit => return Ok(()),
                            }
                        }
                        Err(smtp_proto::Error::NeedsMoreData { .. }) => break,
                        Err(err) => {
                            let text = match err {
                                smtp_proto::Error::UnknownCommand => "500 5.5.1 Unknown command\r\n".to_owned(),
                                smtp_proto::Error::ResponseTooLong => "500 5.5.6 Line too long\r\n".to_owned(),
                                smtp_proto::Error::InvalidSenderAddress => {
                                    "501 5.1.7 Bad sender address syntax\r\n".to_owned()
                                }
                                smtp_proto::Error::InvalidRecipientAddress => {
                                    "501 5.1.3 Bad recipient address syntax\r\n".to_owned()
                                }
                                other => format!("501 5.5.4 {other}\r\n"),
                            };
                            if self.error(&text).await? {
                                return Ok(());
                            }
                        }
                    },
                    State::Data(receiver) => {
                        let done = receiver.ingest(&mut bytes, &mut self.message);
                        if done {
                            state = State::Command(RequestReceiver::default());
                            let message = std::mem::take(&mut self.message);
                            self.finish_message(message).await?;
                        } else if self.message.len() > max_size {
                            self.message = Vec::new();
                            self.message_too_big = true;
                            state = State::DataDiscard(DummyDataReceiver::new_data(receiver));
                        } else {
                            break;
                        }
                    }
                    State::DataDiscard(receiver) => {
                        if receiver.ingest(&mut bytes) {
                            state = State::Command(RequestReceiver::default());
                            self.reset_transaction();
                            self.reply("552 5.3.4 Message too big\r\n").await?;
                        } else {
                            break;
                        }
                    }
                    State::Bdat(receiver) => {
                        if receiver.ingest(&mut bytes, &mut self.message) {
                            let last = receiver.is_last;
                            state = State::Command(RequestReceiver::default());
                            if self.message.len() > max_size {
                                self.message = Vec::new();
                                self.message_too_big = true;
                            }
                            if !last {
                                self.reply("250 2.0.0 Chunk received\r\n").await?;
                            } else if self.message_too_big {
                                self.reset_transaction();
                                self.reply("552 5.3.4 Message too big\r\n").await?;
                            } else {
                                let message = std::mem::take(&mut self.message);
                                self.finish_message(message).await?;
                            }
                        } else {
                            break;
                        }
                    }
                    State::BdatDiscard(receiver) => {
                        if receiver.ingest(&mut bytes) {
                            state = State::Command(RequestReceiver::default());
                            self.reply("503 5.5.1 Send MAIL and RCPT first\r\n").await?;
                        } else {
                            break;
                        }
                    }
                    State::Auth(_, line) => {
                        if !line.ingest(&mut bytes) {
                            break;
                        }
                        let text = String::from_utf8_lossy(&std::mem::take(&mut line.buf)).trim().to_owned();
                        let State::Auth(step, _) =
                            std::mem::replace(&mut state, State::Command(RequestReceiver::default()))
                        else {
                            unreachable!()
                        };
                        match self.auth_line(step, &text).await? {
                            Next::Auth(step) => state = State::Auth(step, LineReceiver::new(())),
                            Next::Quit => return Ok(()),
                            _ => {}
                        }
                    }
                }
                if bytes.as_slice().is_empty() {
                    break;
                }
            }
        }
    }

    /// Counts a protocol error. Returns true when the connection was closed.
    async fn error(&mut self, text: &str) -> std::io::Result<bool> {
        self.errors += 1;
        if self.errors >= MAX_ERRORS {
            self.reply("421 4.7.0 Too many errors, closing connection\r\n").await?;
            return Ok(true);
        }
        self.reply(text).await?;
        Ok(false)
    }

    fn reset_transaction(&mut self) {
        self.envelope = None;
        self.recipients.clear();
        self.message.clear();
        self.message_too_big = false;
    }

    async fn command(&mut self, request: Request<String>) -> std::io::Result<Next> {
        match request {
            Request::Ehlo { host } => self.ehlo(host, true).await,
            Request::Helo { host } => self.ehlo(host, false).await,
            Request::StartTls => {
                if self.is_tls() || self.smtp.inner.server_tls.is_none() {
                    self.error("503 5.5.1 TLS is not available\r\n").await?;
                    return Ok(Next::Continue);
                }
                self.reply("220 2.0.0 Ready to start TLS\r\n").await?;
                Ok(Next::StartTls)
            }
            Request::Auth { mechanism, initial_response } => self.auth(mechanism, initial_response).await,
            Request::Mail { from } => self.mail(from).await,
            Request::Rcpt { to } => self.rcpt(to).await,
            Request::Data => {
                if self.recipients.is_empty() {
                    self.error("503 5.5.1 Send MAIL and RCPT first\r\n").await?;
                    return Ok(Next::Continue);
                }
                self.reply("354 Start mail input; end with <CRLF>.<CRLF>\r\n").await?;
                Ok(Next::Data)
            }
            Request::Bdat { chunk_size, is_last } => {
                if self.recipients.is_empty() {
                    return Ok(Next::BdatDiscard(chunk_size));
                }
                Ok(Next::Bdat { size: chunk_size, last: is_last })
            }
            Request::Rset => {
                self.reset_transaction();
                self.reply("250 2.0.0 OK\r\n").await?;
                Ok(Next::Continue)
            }
            Request::Noop { .. } => {
                self.reply("250 2.0.0 OK\r\n").await?;
                Ok(Next::Continue)
            }
            Request::Quit => {
                let _ = self.reply("221 2.0.0 Bye, see you soon\r\n").await;
                Ok(Next::Quit)
            }
            Request::Vrfy { .. } => {
                self.reply("252 2.5.2 Cannot verify, but will accept the message and try\r\n").await?;
                Ok(Next::Continue)
            }
            Request::Help { .. } => {
                self.reply("214 2.0.0 See https://github.com/MinifyX/UwUMail-Server\r\n").await?;
                Ok(Next::Continue)
            }
            Request::Lhlo { .. }
            | Request::Expn { .. }
            | Request::Etrn { .. }
            | Request::Atrn { .. }
            | Request::Burl { .. } => {
                self.error("502 5.5.1 Command not implemented\r\n").await?;
                Ok(Next::Continue)
            }
        }
    }

    fn auth_available(&self) -> bool {
        self.kind.is_submission() && (self.is_tls() || !self.smtp.inner.live().smtp.require_tls_for_auth)
    }

    async fn ehlo(&mut self, host: String, extended: bool) -> std::io::Result<Next> {
        self.reset_transaction();
        let smtp = self.smtp.clone();
        let ctx = &smtp.inner;
        let hostname = ctx.hostname.clone();
        if !extended {
            self.helo = Some(host);
            self.reply(&format!("250 {hostname}\r\n")).await?;
            return Ok(Next::Continue);
        }
        let mut lines = vec![
            format!("{hostname} Hello {host}"),
            "PIPELINING".into(),
            "8BITMIME".into(),
            "SMTPUTF8".into(),
            "ENHANCEDSTATUSCODES".into(),
            "CHUNKING".into(),
            format!("SIZE {}", ctx.live().smtp.max_message_size),
        ];
        if !self.is_tls() && ctx.server_tls.is_some() {
            lines.push("STARTTLS".into());
        }
        if self.auth_available() && self.account.is_none() {
            lines.push("AUTH PLAIN LOGIN".into());
        }
        let mut response = String::new();
        for (index, line) in lines.iter().enumerate() {
            let separator = if index + 1 == lines.len() { ' ' } else { '-' };
            response.push_str(&format!("250{separator}{line}\r\n"));
        }
        self.helo = Some(host);
        self.reply(&response).await?;
        Ok(Next::Continue)
    }

    async fn start_tls(&mut self) -> std::io::Result<()> {
        let tls = self.smtp.inner.server_tls.clone().expect("checked before offering STARTTLS");
        let Some(Stream::Plain(socket)) = self.stream.take() else {
            return Err(std::io::Error::other("STARTTLS on a stream that is not plain"));
        };
        let accepted = timeout(Duration::from_secs(30), TlsAcceptor::from(tls).accept(socket))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
        self.stream = Some(Stream::Server(Box::new(accepted)));
        // RFC 3207: forget everything learned before the handshake.
        self.helo = None;
        self.reset_transaction();
        Ok(())
    }

    async fn auth(&mut self, mechanism: u64, initial_response: String) -> std::io::Result<Next> {
        if !self.kind.is_submission() {
            self.error("503 5.5.1 Authentication is only available on the submission ports\r\n").await?;
            return Ok(Next::Continue);
        }
        if !self.auth_available() {
            self.error("538 5.7.11 Encryption required, use STARTTLS first\r\n").await?;
            return Ok(Next::Continue);
        }
        if self.account.is_some() {
            self.error("503 5.5.1 Already authenticated\r\n").await?;
            return Ok(Next::Continue);
        }
        if self.envelope.is_some() {
            self.error("503 5.5.1 AUTH is not allowed during a mail transaction\r\n").await?;
            return Ok(Next::Continue);
        }
        if self.smtp.inner.auth_limiter.is_blocked(self.peer) {
            self.reply("421 4.7.0 Too many failed logins from your network, try again later\r\n").await?;
            return Ok(Next::Quit);
        }
        let initial = initial_response.trim();
        let initial = if initial == "=" { "" } else { initial };
        match mechanism {
            AUTH_PLAIN if initial.is_empty() => {
                self.reply("334 \r\n").await?;
                Ok(Next::Auth(AuthStep::PlainResponse))
            }
            AUTH_PLAIN => self.auth_plain(initial).await,
            AUTH_LOGIN if initial.is_empty() => {
                self.reply("334 VXNlcm5hbWU6\r\n").await?;
                Ok(Next::Auth(AuthStep::LoginUsername))
            }
            AUTH_LOGIN => match decode_utf8(initial) {
                Some(username) => {
                    self.reply("334 UGFzc3dvcmQ6\r\n").await?;
                    Ok(Next::Auth(AuthStep::LoginPassword(username)))
                }
                None => {
                    self.error("501 5.5.2 Invalid base64 data\r\n").await?;
                    Ok(Next::Continue)
                }
            },
            _ => {
                self.error("504 5.5.4 Authentication mechanism not supported\r\n").await?;
                Ok(Next::Continue)
            }
        }
    }

    async fn auth_line(&mut self, step: AuthStep, line: &str) -> std::io::Result<Next> {
        if line == "*" {
            self.error("501 5.0.0 Authentication cancelled\r\n").await?;
            return Ok(Next::Continue);
        }
        match step {
            AuthStep::PlainResponse => self.auth_plain(line).await,
            AuthStep::LoginUsername => match decode_utf8(line) {
                Some(username) => {
                    self.reply("334 UGFzc3dvcmQ6\r\n").await?;
                    Ok(Next::Auth(AuthStep::LoginPassword(username)))
                }
                None => {
                    self.error("501 5.5.2 Invalid base64 data\r\n").await?;
                    Ok(Next::Continue)
                }
            },
            AuthStep::LoginPassword(username) => match decode_utf8(line) {
                Some(password) => self.check_credentials(&username, &password).await,
                None => {
                    self.error("501 5.5.2 Invalid base64 data\r\n").await?;
                    Ok(Next::Continue)
                }
            },
        }
    }

    async fn auth_plain(&mut self, response: &str) -> std::io::Result<Next> {
        let decoded = BASE64.decode(response).ok();
        let parts: Option<Vec<String>> = decoded.and_then(|bytes| {
            let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
            (parts.len() == 3).then(|| parts.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect())
        });
        let Some(parts) = parts else {
            self.error("501 5.5.2 Invalid PLAIN authentication data\r\n").await?;
            return Ok(Next::Continue);
        };
        let (authzid, authcid, password) = (&parts[0], &parts[1], &parts[2]);
        if !authzid.is_empty() && !authzid.eq_ignore_ascii_case(authcid) {
            self.error("535 5.7.8 Authorization identity must match the login\r\n").await?;
            return Ok(Next::Continue);
        }
        self.check_credentials(&authcid.clone(), &password.clone()).await
    }

    async fn check_credentials(&mut self, login: &str, password: &str) -> std::io::Result<Next> {
        let smtp = self.smtp.clone();
        let ctx = &smtp.inner;
        let peer = self.peer.to_string();
        match ctx.store.authenticate_mail(login, password, AppScope::Smtp, "smtp", &peer).await {
            Ok(MailAuth::Ok { account, app_password }) => {
                ctx.auth_limiter.record_success(self.peer);
                tracing::info!(login = %account.login, peer = %self.peer, app_password = app_password.is_some(), "smtp login");
                self.account = Some(account);
                self.reply("235 2.7.0 Authentication succeeded\r\n").await?;
                Ok(Next::Continue)
            }
            Ok(MailAuth::Denied(reason)) => {
                // A phone still using the right account password should not lock out its network.
                if reason != MailAuthDenied::AppPasswordRequired {
                    ctx.auth_limiter.record_failure(self.peer);
                }
                self.auth_failures += 1;
                tracing::warn!(%login, peer = %self.peer, %reason, "failed smtp login");
                tokio::time::sleep(Duration::from_secs(1)).await;
                if self.auth_failures >= MAX_AUTH_FAILURES {
                    self.reply("421 4.7.0 Too many failed logins, closing connection\r\n").await?;
                    return Ok(Next::Quit);
                }
                self.reply("535 5.7.8 Authentication credentials invalid\r\n").await?;
                Ok(Next::Continue)
            }
            Err(err) => {
                tracing::error!(%err, "authentication failed internally");
                self.reply("454 4.7.0 Temporary authentication failure\r\n").await?;
                Ok(Next::Continue)
            }
        }
    }

    async fn mail(&mut self, from: MailFrom<String>) -> std::io::Result<Next> {
        if self.helo.is_none() {
            self.error("503 5.5.1 Say EHLO first\r\n").await?;
            return Ok(Next::Continue);
        }
        if self.envelope.is_some() {
            self.error("503 5.5.1 Nested MAIL command\r\n").await?;
            return Ok(Next::Continue);
        }
        if self.kind.is_submission() && self.account.is_none() {
            self.error("530 5.7.0 Authentication required\r\n").await?;
            return Ok(Next::Continue);
        }
        let smtp = self.smtp.clone();
        let ctx = &smtp.inner;
        if from.size > ctx.live().smtp.max_message_size {
            self.error("552 5.3.4 Message too big\r\n").await?;
            return Ok(Next::Continue);
        }
        let address = from.address.trim().to_owned();
        if let Some(account) = &self.account {
            let allowed =
                !address.is_empty() && ctx.store.account_owns_address(account.id, &address).await.unwrap_or(false);
            if !allowed {
                let text = format!("553 5.7.1 You are not allowed to send as <{address}>\r\n");
                self.error(&text).await?;
                return Ok(Next::Continue);
            }
        }
        self.envelope = Some(Envelope { address, size: from.size, env_id: from.env_id });
        self.reply("250 2.1.0 Sender OK\r\n").await?;
        Ok(Next::Continue)
    }

    async fn rcpt(&mut self, to: RcptTo<String>) -> std::io::Result<Next> {
        if self.envelope.is_none() {
            self.error("503 5.5.1 Send MAIL first\r\n").await?;
            return Ok(Next::Continue);
        }
        let smtp = self.smtp.clone();
        let ctx = &smtp.inner;
        if self.recipients.len() >= ctx.live().smtp.max_recipients {
            self.reply("452 4.5.3 Too many recipients\r\n").await?;
            return Ok(Next::Continue);
        }
        let Ok((local, domain)) = uwumail_store::normalize_address(&to.address) else {
            self.error("501 5.1.3 Bad recipient address syntax\r\n").await?;
            return Ok(Next::Continue);
        };
        let address = format!("{local}@{domain}");
        let store = &ctx.store;
        if !self.kind.is_submission()
            && srs::looks_like_srs(&local)
            && store.is_local_domain(&domain).await.unwrap_or(false)
        {
            let original = match srs::secret(store).await {
                Some(secret) => srs::reverse(&secret, &address),
                None => None,
            };
            let is_bounce = self.envelope.as_ref().is_some_and(|envelope| envelope.address.is_empty());
            return match original {
                // Rewritten senders of forwarded mail only take delivery notices.
                Some(original) if is_bounce => {
                    if !self.recipients.iter().any(|r| r.address == address) {
                        self.recipients.push(Recipient {
                            address,
                            local_account: None,
                            srs_return: Some(original),
                            report: None,
                            forward_to: None,
                            notify_flags: to.flags,
                            orcpt: to.orcpt,
                        });
                    }
                    self.reply("250 2.1.5 Recipient OK\r\n").await?;
                    Ok(Next::Continue)
                }
                Some(_) => {
                    self.error("550 5.7.1 This address only accepts delivery notices\r\n").await?;
                    Ok(Next::Continue)
                }
                None => {
                    let text = format!("550 5.1.1 <{address}>: No such mailbox here\r\n");
                    self.error(&text).await?;
                    Ok(Next::Continue)
                }
            };
        }
        if !self.kind.is_submission()
            && let Ok(Some(kind)) = store.report_recipient(&address).await
        {
            if !self.recipients.iter().any(|r| r.address == address) {
                self.recipients.push(Recipient {
                    address,
                    local_account: None,
                    srs_return: None,
                    report: Some(kind),
                    forward_to: None,
                    notify_flags: to.flags,
                    orcpt: to.orcpt,
                });
            }
            self.reply("250 2.1.5 Recipient OK\r\n").await?;
            return Ok(Next::Continue);
        }
        // On submission the targets are looked up again when the message is handed on.
        if let Ok(Some(targets)) = store.forward_address_targets(&address).await {
            if !self.recipients.iter().any(|r| r.address == address) {
                self.recipients.push(Recipient {
                    address,
                    local_account: None,
                    srs_return: None,
                    report: None,
                    forward_to: Some(targets),
                    notify_flags: to.flags,
                    orcpt: to.orcpt,
                });
            }
            self.reply("250 2.1.5 Recipient OK\r\n").await?;
            return Ok(Next::Continue);
        }
        let local_account = match store.resolve_recipient(&address).await {
            Ok(found) => found,
            Err(err) => {
                tracing::error!(%err, "recipient lookup failed");
                self.reply("451 4.3.0 Temporary lookup failure\r\n").await?;
                return Ok(Next::Continue);
            }
        };
        if local_account.is_none() {
            if store.is_local_domain(&domain).await.unwrap_or(false) {
                let text = format!("550 5.1.1 <{address}>: No such mailbox here\r\n");
                self.error(&text).await?;
                return Ok(Next::Continue);
            }
            if !self.kind.is_submission() {
                self.error("550 5.7.1 Relaying denied\r\n").await?;
                return Ok(Next::Continue);
            }
        }
        if let Some(account_id) = local_account
            && let Ok(Some(account)) = store.account_by_id(account_id).await
            && account.quota_bytes > 0
            && account.used_bytes + self.envelope.as_ref().map_or(0, |e| e.size as i64) > account.quota_bytes
        {
            let text = format!("452 4.2.2 <{address}>: Mailbox is full\r\n");
            self.reply(&text).await?;
            return Ok(Next::Continue);
        }
        if !self.recipients.iter().any(|r| r.address == address) {
            self.recipients.push(Recipient {
                address,
                local_account,
                srs_return: None,
                report: None,
                forward_to: None,
                notify_flags: to.flags,
                orcpt: to.orcpt,
            });
        }
        self.reply("250 2.1.5 Recipient OK\r\n").await?;
        Ok(Next::Continue)
    }

    async fn finish_message(&mut self, raw: Vec<u8>) -> std::io::Result<()> {
        let envelope = self.envelope.take();
        let recipients = std::mem::take(&mut self.recipients);
        let Some(envelope) = envelope else {
            return self.reply("503 5.5.1 Send MAIL first\r\n").await;
        };
        if headers::count(&raw, "Received") > MAX_HOPS {
            return self.reply("554 5.4.6 Too many hops, possible mail loop\r\n").await;
        }
        let raw = headers::normalize_line_endings(&raw);
        let response = if self.kind.is_submission() {
            self.submit(envelope, recipients, raw).await
        } else {
            self.receive(envelope, recipients, raw).await
        };
        self.reply(&response).await
    }

    fn received_header(&self, id: &str, recipient: Option<&str>) -> String {
        let ctx = &self.smtp.inner;
        let private = self.account.is_some() && !ctx.live().smtp.reveal_client_ip;
        // The name a mail app announces often is the device name or a local IP, so it stays private too.
        let helo = if private { "localhost" } else { self.helo.as_deref().unwrap_or("unknown") };
        let tls = self.stream.as_ref().and_then(Stream::tls_description);
        let protocol = match (self.account.is_some(), tls.is_some()) {
            (true, true) => "ESMTPSA",
            (true, false) => "ESMTPA",
            (false, true) => "ESMTPS",
            (false, false) => "ESMTP",
        };
        let client = if private { String::new() } else { format!(" ([{}])", self.peer) };
        let mut header = format!("Received: from {helo}{client}\r\n\tby {} (UwUMail) with {protocol}", ctx.hostname);
        if let Some(tls) = tls {
            header.push_str(&format!("\r\n\t(using {tls})"));
        }
        header.push_str(&format!(" id {id}"));
        if let Some(recipient) = recipient {
            header.push_str(&format!("\r\n\tfor <{recipient}>"));
        }
        header.push_str(&format!(";\r\n\t{}\r\n", Date::now().to_rfc822()));
        header
    }

    /// Mail from another server for our own people.
    async fn receive(&mut self, envelope: Envelope, recipients: Vec<Recipient>, raw: Vec<u8>) -> String {
        let ctx = self.smtp.inner.clone();
        let id = random_id();
        let raw = headers::strip_forged_auth_results(&raw, &ctx.hostname);
        let helo = self.helo.clone().unwrap_or_default();

        // Behind a trusted relay, check the server that talked to the relay.
        let live = ctx.live();
        let client = if live.trusted_relays.iter().any(|network| network.contains(self.peer)) {
            let found = relay::original_client(&raw, &live.trusted_relays);
            if found.is_none() {
                tracing::warn!(%id, relay = %self.peer, "no readable Received header from the trusted relay, skipping sender checks");
            }
            found
        } else {
            Some((self.peer, helo))
        };

        let verdict = match &client {
            Some((ip, helo)) if live.smtp.verify_senders => {
                Some(checks::verify(&ctx, *ip, helo, &envelope.address, &raw).await)
            }
            _ => None,
        };
        if let Some(checks::Verdict { action: Action::Reject(reason), .. }) = &verdict {
            tracing::info!(%id, from = %envelope.address, %reason, "rejected by DMARC");
            return format!("550 5.7.1 {reason}\r\n");
        }

        // Allowed and blocked senders decide for each recipient before the score does.
        let sender =
            sender_lists::Sender::new(client.as_ref().map(|(ip, _)| *ip), &envelope.address, verdict.as_ref(), &raw);
        let targets: Vec<(i64, String)> = recipients
            .iter()
            .filter_map(|recipient| {
                let (_, domain) = recipient.address.rsplit_once('@')?;
                Some((recipient.local_account?, domain.to_owned()))
            })
            .collect();
        let lists = if targets.is_empty() {
            sender_lists::Lists::default()
        } else {
            sender_lists::load(&ctx, &sender, &targets).await
        };
        let decisions: Vec<Decision> = recipients
            .iter()
            .map(|recipient| match (recipient.local_account, recipient.address.rsplit_once('@')) {
                (Some(account), Some((_, domain))) => lists.decide(&sender, account, domain),
                _ => Decision::None,
            })
            .collect();
        if !decisions.is_empty() && decisions.iter().all(|decision| matches!(decision, Decision::Reject(_))) {
            let listed = decisions[0].entry().map(|entry| entry.value.as_str());
            tracing::info!(%id, from = %envelope.address, listed, "refused by a sender list");
            return "550 5.7.1 Mail from this sender is not accepted here\r\n".into();
        }
        let allowed = decisions.iter().any(|decision| matches!(decision, Decision::Allow(_)));

        // Only mail we can attribute to a sending server is scored; behind a relay that means the
        // server the relay talked to.
        let score = match &client {
            Some((ip, helo)) if live.spam.enabled => {
                spam::score(&ctx, &live.spam, *ip, helo, verdict.as_ref(), &raw).await
            }
            _ => None,
        };
        let outcome = score.as_ref().map_or(spam::Outcome::Deliver, |score| spam::outcome(&live.spam, score.points));
        // What the filter saw, for the log: the score and the rules behind it.
        let spam_score = score.as_ref().map(|score| score.points);
        let spam_tests = score.as_ref().map(spam::Score::tests);
        let quarantined = matches!(&verdict, Some(v) if v.action == Action::Quarantine);
        let junk = quarantined || matches!(outcome, spam::Outcome::Junk | spam::Outcome::Reject);
        // What the score means for each person without a listed sender: their own Bayes knowledge and
        // word lists add points, and their own limits say what the sum means.
        let limits = match &score {
            Some(_) if !quarantined && !targets.is_empty() => {
                let accounts = targets.iter().map(|(account, _)| *account).collect();
                ctx.store.spam_limits_for(accounts).await.unwrap_or_else(|err| {
                    tracing::warn!(%id, %err, "reading the spam limits failed, using the server's");
                    Default::default()
                })
            }
            _ => Default::default(),
        };
        let mut personal: Vec<Option<(f32, spam::Outcome)>> = Vec::with_capacity(recipients.len());
        for (recipient, decision) in recipients.iter().zip(&decisions) {
            let theirs = match (&score, recipient.local_account, decision) {
                (Some(score), Some(account_id), Decision::None) if !quarantined => {
                    let domain = recipient.address.rsplit_once('@').map_or("", |(_, domain)| domain);
                    let own = spam::personal_bayes_points(&ctx, &live.spam, score, account_id).await
                        + spam::personal_word_points(&ctx, score, account_id, domain).await;
                    let limits = limits.get(&account_id).copied().unwrap_or_default();
                    Some((own, spam::personal_outcome(&live.spam, limits, score.points + own)))
                }
                _ => None,
            };
            personal.push(theirs);
        }
        // Refused when the server's limit says so, or when every recipient's own limit or list does, or
        // nobody but forwarding addresses would get it, which pass no spam on. Otherwise a recipient who
        // allowed the sender still gets it and everyone else finds it in Junk.
        let everyone_refuses = !recipients.is_empty()
            && recipients.iter().zip(&decisions).zip(&personal).all(|((recipient, decision), theirs)| match decision {
                Decision::Reject(_) => true,
                Decision::None if recipient.forward_to.is_some() => junk,
                Decision::None => {
                    recipient.srs_return.is_none()
                        && recipient.report.is_none()
                        && matches!(theirs, Some((_, spam::Outcome::Reject)))
                }
                _ => false,
            });
        if (outcome == spam::Outcome::Reject && !allowed) || everyone_refuses {
            tracing::info!(
                %id,
                from = %envelope.address,
                score = spam_score,
                tests = spam_tests.as_deref(),
                "refused as spam"
            );
            return "550 5.7.1 This message looks like spam\r\n".into();
        }
        if outcome == spam::Outcome::Suspicious
            && let Some((ip, _)) = &client
        {
            let mut waiting = Vec::new();
            for (recipient, decision) in recipients.iter().zip(&decisions) {
                // An allowed sender never waits, and neither does the message for the others then.
                if matches!(decision, Decision::Allow(_)) {
                    continue;
                }
                let wait = spam::greylist_wait(&ctx, &live.spam, *ip, &envelope.address, &recipient.address).await;
                if let Some(seconds) = wait {
                    waiting.push(seconds);
                }
            }
            // Hold the message only while every recipient is still waiting. Once one of them may
            // have it, delivering to all of them keeps the retry from arriving twice.
            if !recipients.is_empty()
                && waiting.len() == recipients.len()
                && let Some(seconds) = waiting.into_iter().min()
            {
                let minutes = ((seconds + 59) / 60).max(1);
                tracing::info!(
                    %id,
                    from = %envelope.address,
                    %seconds,
                    score = spam_score,
                    tests = spam_tests.as_deref(),
                    "greylisted"
                );
                return format!("451 4.7.1 Please try again in {minutes} minutes\r\n");
            }
        }

        // Our verdict replaces whatever the message brought along.
        let raw = if score.is_some() { headers::strip_spam_verdicts(&raw) } else { raw };

        let single = (recipients.len() == 1).then(|| recipients[0].address.as_str());
        let mut message = self.received_header(&id, single).into_bytes();
        if let Some(verdict) = &verdict {
            message.extend_from_slice(verdict.header.as_bytes());
        }
        if let Some(score) = &score {
            message.extend_from_slice(spam::headers(score, junk, live.spam.junk_score).as_bytes());
        }
        message.extend_from_slice(&raw);

        let mut delivered = 0;
        let mut inbox_accounts = Vec::new();
        let mut failed: Vec<FailedRecipient> = Vec::new();
        let mut temporary = false;
        let mut seen_accounts = Vec::new();
        let mut returned = Vec::new();
        for ((recipient, decision), theirs) in recipients.iter().zip(&decisions).zip(&personal) {
            if let Some(original) = &recipient.srs_return {
                returned.push(NewQueueRecipient { address: original.clone(), notify_flags: 0, orcpt: None });
                continue;
            }
            if let Some(kind) = recipient.report {
                let authenticated = verdict.as_ref().is_some_and(|verdict| verdict.dmarc_passed);
                tokio::spawn(reports::receive(
                    ctx.clone(),
                    kind,
                    recipient.address.clone(),
                    raw.clone(),
                    authenticated,
                ));
                delivered += 1;
                continue;
            }
            if let Some(targets) = &recipient.forward_to {
                if junk {
                    tracing::info!(%id, to = %recipient.address, "not passing spam on from a forwarding address");
                } else {
                    let forwarder = forward::Forwarder { name: &recipient.address, account_id: None };
                    forward::send(&ctx, forwarder, &recipient.address, &envelope.address, &message, targets).await;
                }
                delivered += 1;
                continue;
            }
            let Some(account_id) = recipient.local_account else { continue };
            if seen_accounts.contains(&account_id) {
                continue;
            }
            seen_accounts.push(account_id);
            // A listed sender goes where the list says, even out of a DMARC quarantine. Otherwise what a
            // person taught their own Bayes filter and their own limits can move the message into or out of
            // Junk for them, and a DMARC quarantine stays a quarantine.
            let junk = match decision {
                Decision::Allow(entry) | Decision::Junk(entry) | Decision::Reject(entry) => {
                    let listed = !matches!(decision, Decision::Allow(_));
                    if listed != junk {
                        tracing::info!(%id, account = account_id, junk = listed, listed = %entry.value, "moved by a sender list");
                    }
                    listed
                }
                Decision::None => match theirs {
                    Some((own, theirs)) => {
                        let moved = matches!(theirs, spam::Outcome::Junk | spam::Outcome::Reject);
                        if moved != junk {
                            tracing::info!(%id, account = account_id, junk = moved, points = own, "moved by a person's own filter");
                        }
                        moved
                    }
                    None => junk,
                },
            };
            // Suspicious mail is never forwarded; it stays in Junk.
            let plan = if junk {
                forward::Plan { keep_copy: true, targets: Vec::new() }
            } else {
                forward::plan(&ctx, account_id).await
            };
            if !plan.targets.is_empty()
                && let Ok(Some(account)) = ctx.store.account_by_id(account_id).await
            {
                let forwarder = forward::Forwarder { name: &account.login, account_id: Some(account.id) };
                forward::send(&ctx, forwarder, &recipient.address, &envelope.address, &message, &plan.targets).await;
            }
            if !plan.keep_copy {
                delivered += 1;
                inbox_accounts.push(account_id);
                continue;
            }
            let mailboxes = vec![MailboxTarget::Role(if junk { MailboxRole::Junk } else { MailboxRole::Inbox })];
            let request =
                IngestRequest { account_id, raw: message.clone(), mailboxes, keywords: vec![], received_at: None };
            match ctx.store.ingest(request).await {
                Ok(_) => {
                    delivered += 1;
                    if !junk {
                        inbox_accounts.push(account_id);
                    }
                }
                Err(StoreError::QuotaExceeded) => failed.push(FailedRecipient {
                    address: recipient.address.clone(),
                    error: "552 5.2.2 Mailbox is full".into(),
                }),
                Err(err) => {
                    tracing::error!(%id, %err, "storing an incoming message failed");
                    temporary = true;
                    failed.push(FailedRecipient {
                        address: recipient.address.clone(),
                        error: "451 4.3.0 Temporary storage failure".into(),
                    });
                }
            }
        }

        if !returned.is_empty() {
            let lifetime = ctx.live().delivery.max_lifetime_hours as i64 * 3600;
            let count = returned.len();
            match ctx.store.enqueue("", returned, &message, None, None, lifetime).await {
                Ok(_) => delivered += count,
                Err(err) => {
                    tracing::error!(%id, %err, "passing on a bounce for forwarded mail failed");
                    temporary = true;
                }
            }
        }
        tracing::info!(
            %id,
            from = %envelope.address,
            recipients = recipients.len(),
            delivered,
            junk,
            score = spam_score,
            tests = spam_tests.as_deref(),
            "received message"
        );
        // Count the message for whoever sent it, so a sender that keeps behaving gets the benefit
        // of the doubt next time, and one that keeps ending up in Junk stops getting it.
        if let (Some(score), Some((ip, _))) = (&score, &client)
            && delivered > 0
        {
            let subject = spam::reputation_subject(*ip, verdict.as_ref());
            let points = score.points_on_its_own();
            let counts_as_junk =
                quarantined || matches!(spam::outcome(&live.spam, points), spam::Outcome::Junk | spam::Outcome::Reject);
            // Every recipient stores the same bytes, so this names the message a person may mark later.
            let stored = uwumail_store::BlobHash::of(&message);
            if let Err(err) = ctx.store.record_delivery(stored.clone(), subject, counts_as_junk).await {
                tracing::warn!(%id, %err, "counting a message for the sender reputation failed");
            }
            // Clear cases teach the whole server's Bayes filter without anyone marking them: very spammy
            // on the message's own merits, or vouched for by DMARC with nothing against it.
            let dmarc_passed = verdict.as_ref().is_some_and(|verdict| verdict.dmarc_passed);
            // Nobody allowed the sender for the server to learn their mail as spam.
            let spam = points >= spam::AUTOLEARN_SPAM && !allowed;
            let wanted = !junk && dmarc_passed && points <= 0.0;
            if live.spam.bayes
                && (spam || wanted)
                && let Err(err) = ctx.store.queue_bayes_learning(stored, None, spam).await
            {
                tracing::warn!(%id, %err, "queueing a clear case for the Bayes filter failed");
            }
        }

        if delivered == 0 {
            return if temporary {
                "451 4.3.0 Temporary storage failure, please try again later\r\n".into()
            } else {
                "552 5.2.2 Mailbox is full\r\n".into()
            };
        }
        for account_id in inbox_accounts {
            vacation::maybe_reply(&ctx, account_id, &envelope.address, &message).await;
        }
        let sender_verified = verdict.as_ref().is_none_or(|v| v.sender_verified);
        if !failed.is_empty() && sender_verified {
            dsn::bounce(&ctx, &envelope.address, &message, &failed).await;
        }
        format!("250 2.0.0 Message accepted as {id}\r\n")
    }

    /// Mail from one of our people, to anyone.
    async fn submit(&mut self, envelope: Envelope, recipients: Vec<Recipient>, raw: Vec<u8>) -> String {
        let account = self.account.clone().expect("MAIL requires authentication on submission ports");
        let trace = self.received_header(&random_id(), None);
        let submission = Submission {
            account,
            mail_from: envelope.address,
            recipients: recipients
                .into_iter()
                .map(|r| SubmissionRecipient { address: r.address, notify_flags: r.notify_flags, orcpt: r.orcpt })
                .collect(),
            raw,
            env_id: envelope.env_id,
            trace: Some(trace),
        };
        match self.smtp.submit(submission).await {
            Ok(submitted) => format!("250 2.0.0 Message queued as {}\r\n", submitted.id),
            Err(SubmitError::NoFrom) => "550 5.6.0 The message has no From header\r\n".into(),
            Err(SubmitError::ForbiddenFrom(address)) => {
                format!("550 5.7.1 You are not allowed to send as <{address}>\r\n")
            }
            Err(SubmitError::AmbiguousSender) => "550 5.6.0 A message may have only one Sender\r\n".into(),
            Err(SubmitError::InvalidRecipient(address)) => format!("501 5.1.3 <{address}> is not a valid address\r\n"),
            Err(SubmitError::NoRecipients) => "503 5.5.1 Send RCPT first\r\n".into(),
            Err(SubmitError::NobodyAccepted) => "552 5.2.2 No recipient could take the message\r\n".into(),
            Err(SubmitError::Queue(err)) => {
                tracing::error!(%err, "queueing a message failed");
                "451 4.3.0 Could not queue the message, please try again\r\n".into()
            }
        }
    }
}

fn decode_utf8(value: &str) -> Option<String> {
    BASE64.decode(value.trim()).ok().and_then(|bytes| String::from_utf8(bytes).ok())
}
