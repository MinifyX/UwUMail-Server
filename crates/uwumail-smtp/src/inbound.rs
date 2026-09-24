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
    NewSpamLogEntry, ReportKind, SpamAction, SpamLogHit, SpamLogRecipient, StoreError,
};

use crate::checks::{self, Action};
use crate::dsn::{self, FailedRecipient};
use crate::sender_lists::{self, Decision};
use crate::stream::{BoxIo, Stream};
use crate::submission::{Submission, SubmissionRecipient, SubmitError};
use crate::{Smtp, clamav, fetched, forward, headers, random_id, relay, reports, rules, spam, srs, vacation};

const MAX_HOPS: usize = 50;
const MAX_ERRORS: u32 = 10;
const MAX_AUTH_FAILURES: u32 = 3;
/// The most of a subject the spam history keeps. A longer one explains nothing more, and this is
/// the one place where the text of someone's mail is written down at all.
const SPAM_LOG_SUBJECT_MAX: usize = 128;

/// Cuts a value down without splitting a character in half.
fn shorten(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        Some((at, _)) => value[..at].to_owned(),
        None => value.to_owned(),
    }
}

/// What the spam history keeps about one decision. It is gathered as the receiving pipeline learns
/// it, so most of it is missing where a message is turned away before it was ever scored.
struct SpamNote<'a> {
    id: &'a str,
    action: SpamAction,
    envelope: &'a Envelope,
    raw: &'a [u8],
    client: Option<&'a (IpAddr, String)>,
    verdict: Option<&'a checks::Verdict>,
    score: Option<&'a spam::Score>,
    recipients: Vec<SpamLogRecipient>,
    /// Names the stored message, so what a person says about it later can be found again.
    blob_hash: Option<String>,
    /// What the virus scanner found, when it found something.
    virus: Option<&'a str>,
}

/// Every recipient with the same outcome, for the decisions that apply to the whole message.
fn all_recipients(recipients: &[Recipient], action: SpamAction) -> Vec<SpamLogRecipient> {
    recipients
        .iter()
        .map(|recipient| SpamLogRecipient {
            address: recipient.address.clone(),
            action: action.as_str().to_owned(),
            mailbox: None,
        })
        .collect()
}

/// Writes down what the filter decided, for the history under Server → Spam filter.
///
/// The four ways a message is turned away — DMARC, a sender list, the score, greylisting — leave no
/// other trace at all: they never reach a mailbox, and the running log is empty after a restart.
/// A failure here is only logged; a history that cannot be written is worth less than the mail it
/// would have described.
async fn note_spam(ctx: &crate::Context, config: &crate::config::SpamLogConfig, note: SpamNote<'_>) {
    if !config.enabled {
        return;
    }
    // The subject of mail that arrived normally explains nothing and is the most telling thing we
    // could keep, so it stays out unless an admin asked for it. What was held back keeps it.
    let subject = (note.action.held_back() || config.clean_subjects)
        .then(|| {
            note.score
                .map(|score| score.subject.clone())
                .filter(|subject| !subject.is_empty())
                .or_else(|| headers::first_value(note.raw, "Subject"))
        })
        .flatten()
        .map(|subject| shorten(&subject, SPAM_LOG_SUBJECT_MAX));
    // The rules that fired, and for a virus the one thing there is to say about it: its name.
    let mut hits: Vec<SpamLogHit> = note
        .score
        .map(|score| {
            score
                .hits
                .iter()
                .map(|hit| SpamLogHit { rule: hit.rule.to_owned(), points: hit.points, detail: hit.detail.clone() })
                .collect()
        })
        .unwrap_or_default();
    if let Some(name) = note.virus {
        hits.push(SpamLogHit { rule: "VIRUS".into(), points: 0.0, detail: Some(name.to_owned()) });
    }
    let entry = NewSpamLogEntry {
        smtp_id: note.id.to_owned(),
        message_id: headers::first_value(note.raw, "Message-ID").map(|id| shorten(&id, 200)),
        action: note.action.as_str().to_owned(),
        envelope_from: shorten(&note.envelope.address, 320),
        header_from: headers::first_value(note.raw, "From").map(|from| shorten(&from, 320)).unwrap_or_default(),
        subject,
        client_ip: note.client.map(|(ip, _)| ip.to_string()).unwrap_or_default(),
        helo: note.client.map(|(_, helo)| shorten(helo, 255)).unwrap_or_default(),
        reverse_name: note.score.and_then(|score| score.reverse_name.clone()),
        size: note.raw.len() as i64,
        score: note.score.map(|score| score.points),
        hits,
        // The whole Authentication-Results line, which says what SPF, DKIM and DMARC found.
        auth: note.verdict.map(|verdict| verdict.header.trim().to_owned()).filter(|line| !line.is_empty()),
        blob_hash: note.blob_hash,
        recipients: note.recipients,
    };
    if let Err(err) = ctx.store.add_spam_log(entry).await {
        tracing::warn!(id = %note.id, %err, "writing the spam history failed");
    }
}

/// A greylisted message, as it is kept for the people it was addressed to.
struct HeldMessage<'a> {
    id: &'a str,
    envelope: &'a Envelope,
    recipients: &'a [Recipient],
    /// What the sender wrote, for the headers worth showing.
    raw: &'a [u8],
    /// What would have been delivered, our own headers and all.
    message: &'a [u8],
    raw_hash: &'a str,
    message_id: Option<&'a str>,
    client_ip: String,
    score: Option<f32>,
    /// What the filter already read out of the message, if it read it.
    subject: Option<String>,
}

/// Keeps a greylisted message for everyone here it was meant for, so it is not simply gone while
/// its sender is asked to come back.
///
/// Only accounts on this server get a copy: a forwarding address has no page to look at it on, and
/// an address that is only passing mail through has no business holding it. Failures are logged and
/// never change the answer — the sender is being asked to come back either way, and a message we
/// could not keep is exactly the greylisting we had before this existed.
async fn hold_greylisted(ctx: &crate::Context, held: HeldMessage<'_>) {
    if held.message.len() as i64 > uwumail_store::MAX_HELD_SIZE {
        tracing::debug!(id = %held.id, size = held.message.len(), "a greylisted message is too large to keep");
        return;
    }
    let subject = held
        .subject
        .filter(|subject| !subject.is_empty())
        .or_else(|| headers::first_value(held.raw, "Subject"))
        .map(|subject| shorten(&subject, SPAM_LOG_SUBJECT_MAX));
    let header_from = headers::first_value(held.raw, "From").map(|from| shorten(&from, 320)).unwrap_or_default();
    let message_id = held.message_id.map(|id| shorten(id, 200));
    let mut kept = 0;
    for recipient in held.recipients {
        let Some(account_id) = recipient.local_account else { continue };
        let hold = uwumail_store::NewGreylistHold {
            account_id,
            address: recipient.address.clone(),
            envelope_from: shorten(&held.envelope.address, 320),
            header_from: header_from.clone(),
            subject: subject.clone(),
            message_id: message_id.clone(),
            smtp_id: held.id.to_owned(),
            client_ip: held.client_ip.clone(),
            score: held.score,
            message: held.message.to_vec(),
            raw_hash: held.raw_hash.to_owned(),
            keep_secs: uwumail_store::GREYLIST_WAITING_SECS,
        };
        match ctx.store.hold_greylisted(hold).await {
            Ok(Some(_)) => kept += 1,
            // Already as many waiting as the list would show them; greylisted the old way then.
            Ok(None) => {
                tracing::debug!(id = %held.id, account = account_id, "too many messages waiting already")
            }
            Err(err) => {
                tracing::warn!(id = %held.id, account = account_id, %err, "keeping a greylisted message failed")
            }
        }
    }
    if kept > 0 {
        tracing::debug!(id = %held.id, kept, "kept a greylisted message for its recipients");
    }
}

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

/// What became of a message this server fetched from a mailbox elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// It is here now, in the inbox or in Junk, and can be dealt with at the provider.
    Kept,
    /// Not this time: greylisting asked for it later, or the mailbox was full. It stays where it is
    /// and is offered again on the next run, which is what the answer asks for.
    Later(String),
    /// Judged and refused for good, the same way it would have been refused at the door: a virus,
    /// a blocked sender, a DMARC policy that rejects, a score over the limit. It is not brought
    /// here, and at the provider it is dealt with exactly like a message that did arrive -- marked
    /// read or deleted, by what the mailbox is set to. This server has made its decision, and
    /// leaving the message behind would only fill a mailbox nobody reads with what was already
    /// thrown away here.
    Refused(String),
    /// Nothing is wrong with the message: this server has nowhere to put it, because the mailbox
    /// it fetches into is gone or its address takes no mail. That is a mistake on this side, and
    /// somebody else's mail must not be deleted over it -- so it stays where it is, untouched.
    Nowhere(String),
}

/// Hands a message fetched from another provider's mailbox to the same pipeline that mail from
/// other servers goes through: the same checks, the same filter, the same lists, the same
/// forwarding, the same history.
///
/// The envelope is rebuilt from what is left of it. The sender comes from the `Return-Path` the
/// provider wrote, which is the address a bounce would have gone to; without one the message
/// counts as coming from nobody, like a bounce does. The recipient is the mailbox here that the
/// fetched mailbox belongs to, whatever address the message itself names.
pub async fn deliver_fetched(
    smtp: &Smtp,
    mailbox: fetched::Mailbox,
    from_junk: bool,
    to: String,
    raw: Vec<u8>,
) -> Taken {
    let raw = headers::normalize_line_endings(&raw);
    if headers::count(&raw, "Received") > MAX_HOPS {
        return Taken::Refused("554 5.4.6 Too many hops, possible mail loop".into());
    }
    // Deliver into the owner's mailbox -- unless it has none, in which case the mail follows the
    // redirect an admin set for it, or is turned away if there is none, the same as the door
    // decides for a service without a mailbox (security-audit-0.5.2 S-14). Neither of these is a
    // verdict on the message, so it is left where it is at the provider.
    let deliver_to = match smtp.store().account_by_id(mailbox.account_id).await {
        Ok(Some(account)) if account.has_mailbox() => mailbox.account_id,
        Ok(Some(account)) => match smtp.store().delivery_target(account.id).await.ok().flatten() {
            Some(target) => target,
            None => return Taken::Nowhere("550 5.1.1 This address does not take mail".into()),
        },
        _ => return Taken::Nowhere("550 5.1.1 the mailbox this fetches into does not exist any more".into()),
    };
    let envelope = Envelope { address: fetched::return_path(&raw).unwrap_or_default(), size: raw.len(), env_id: None };
    let recipients = vec![Recipient {
        address: to,
        local_account: Some(deliver_to),
        srs_return: None,
        report: None,
        forward_to: None,
        trap: false,
        notify_flags: 0,
        orcpt: None,
    }];
    let origin = Origin::Fetched { mailbox, from_junk };
    let answer = receive(smtp, &origin, envelope, recipients, raw).await;
    let answer = answer.trim().to_owned();
    // A full mailbox is not a verdict on the message: its owner can make room, and then it can
    // come. At the door RCPT says so with a 4xx; storing it says 552, which would make it a
    // refusal -- and a refused message is cleared at the provider, so a mailbox that ran out of
    // room here would quietly delete everything arriving there. So for fetched mail a full mailbox
    // is "later": the folder waits on it, and it comes as soon as it fits.
    if mailbox_full(&answer) {
        return Taken::Later(answer);
    }
    match answer.as_bytes().first() {
        Some(b'2') => Taken::Kept,
        Some(b'4') => Taken::Later(answer),
        _ => Taken::Refused(answer),
    }
}

/// Whether an answer says the mailbox is full: the enhanced status code X.2.2 of RFC 3463, whether
/// it came as 4.2.2 or 5.2.2.
fn mailbox_full(answer: &str) -> bool {
    answer.split_whitespace().nth(1).is_some_and(|code| code.ends_with(".2.2"))
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

pub(crate) struct Envelope {
    address: String,
    size: usize,
    env_id: Option<String>,
}

pub(crate) struct Recipient {
    address: String,
    local_account: Option<i64>,
    /// A bounce for a forwarded message, to pass on to this original sender.
    srs_return: Option<String>,
    /// DMARC or TLS reports for one of our domains, read by the server itself.
    report: Option<ReportKind>,
    /// A forwarding address without a mailbox: where its mail goes, `Some(account id)` for people here.
    forward_to: Option<Vec<(String, Option<i64>)>>,
    /// An address that exists only to catch spam: taken, learned from, never delivered.
    trap: bool,
    notify_flags: u64,
    orcpt: Option<String>,
}

/// Where a message reached this server.
///
/// Everything below this point treats both the same way: the same checks, the same filter, the same
/// sender lists, the same forwarding and the same history. What differs is only what can be known
/// about where the message came from, which is what [`Origin::client`] answers.
pub(crate) enum Origin {
    /// An SMTP client handed it in.
    Client { peer: IpAddr, helo: String, tls: Option<String>, submitted: bool },
    /// This server took it out of a mailbox at another provider.
    Fetched { mailbox: fetched::Mailbox, from_junk: bool },
}

impl Origin {
    /// The address and greeting of the server that sent the message, when one can be known.
    ///
    /// Behind a trusted relay that is the server the relay talked to, read from its Received
    /// header. For a fetched message it is the address the provider wrote down under its own name,
    /// and nothing at all when it wrote none: guessing one would judge an innocent server.
    fn client(
        &self,
        live: &crate::Live,
        raw: &[u8],
        provenance: Option<&fetched::Provenance>,
    ) -> Option<(IpAddr, String)> {
        match self {
            Origin::Client { peer, helo, .. } => {
                if live.trusted_relays.iter().any(|network| network.contains(*peer)) {
                    relay::original_client(raw, &live.trusted_relays)
                } else {
                    Some((*peer, helo.clone()))
                }
            }
            Origin::Fetched { .. } => provenance.and_then(fetched::Provenance::checkable_client),
        }
    }

    /// The trace header this server adds on top, saying how the message got here.
    fn received_header(&self, ctx: &crate::Context, id: &str, recipient: Option<&str>) -> String {
        let mut header = match self {
            Origin::Client { peer, helo, tls, submitted } => {
                let private = *submitted && !ctx.live().smtp.reveal_client_ip;
                // The name a mail app announces often is the device name or a local IP, so it stays
                // private too.
                let helo = if private {
                    "localhost"
                } else if helo.is_empty() {
                    "unknown"
                } else {
                    helo.as_str()
                };
                let protocol = match (submitted, tls.is_some()) {
                    (true, true) => "ESMTPSA",
                    (true, false) => "ESMTPA",
                    (false, true) => "ESMTPS",
                    (false, false) => "ESMTP",
                };
                let client = if private { String::new() } else { format!(" ([{peer}])") };
                let mut header =
                    format!("Received: from {helo}{client}\r\n\tby {} (UwUMail) with {protocol}", ctx.hostname);
                if let Some(tls) = tls {
                    header.push_str(&format!("\r\n\t(using {tls})"));
                }
                header
            }
            Origin::Fetched { mailbox, .. } => format!(
                "Received: from {} (fetched for {})\r\n\tby {} (UwUMail) with IMAP",
                mailbox.host, mailbox.address, ctx.hostname
            ),
        };
        header.push_str(&format!(" id {id}"));
        if let Some(recipient) = recipient {
            header.push_str(&format!("\r\n\tfor <{recipient}>"));
        }
        header.push_str(&format!(";\r\n\t{}\r\n", Date::now().to_rfc822()));
        header
    }
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
                // A BDAT chunk may be empty (`BDAT 0 LAST` ends many a message): it is complete
                // without another byte, so it is taken now rather than after a read that never comes.
                let chunk = matches!(state, State::Bdat(_) | State::BdatDiscard(_));
                if bytes.as_slice().is_empty() && !chunk {
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
                match reason {
                    // A phone still using the right account password should not lock out its network.
                    MailAuthDenied::AppPasswordRequired => {}
                    MailAuthDenied::UnknownLogin => ctx.auth_limiter.record_unknown_login(self.peer),
                    _ => ctx.auth_limiter.record_failure(self.peer),
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
                            trap: false,
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
                    trap: false,
                    notify_flags: to.flags,
                    orcpt: to.orcpt,
                });
            }
            self.reply("250 2.1.5 Recipient OK\r\n").await?;
            return Ok(Next::Continue);
        }
        // A trap address answers exactly like a real one: a trap that says "no such mailbox" is
        // crossed off the spammer's list, and then it catches nothing.
        if !self.kind.is_submission() && ctx.live().spam.traps.iter().any(|trap| trap.eq_ignore_ascii_case(&address)) {
            if !self.recipients.iter().any(|r| r.address == address) {
                self.recipients.push(Recipient {
                    address,
                    local_account: None,
                    srs_return: None,
                    report: None,
                    forward_to: None,
                    trap: true,
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
                    trap: false,
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
        let mut local_account = local_account;
        if let Some(account_id) = local_account
            && let Ok(Some(account)) = store.account_by_id(account_id).await
        {
            // A service that only sends has no mailbox at all. Its mail goes to the address an
            // admin named for it, and without one the address does not take mail -- said here, at
            // the door, so the other side hears it at once instead of guessing.
            if !account.has_mailbox() {
                let Some(target) = store.delivery_target(account.id).await.ok().flatten() else {
                    let text = format!("550 5.1.1 <{address}>: This address does not take mail\r\n");
                    self.error(&text).await?;
                    return Ok(Next::Continue);
                };
                local_account = Some(target);
            } else if account.quota_bytes > 0
                && account.used_bytes + self.envelope.as_ref().map_or(0, |e| e.size as i64) > account.quota_bytes
            {
                let text = format!("452 4.2.2 <{address}>: Mailbox is full\r\n");
                self.reply(&text).await?;
                return Ok(Next::Continue);
            }
        }
        if !self.recipients.iter().any(|r| r.address == address) {
            self.recipients.push(Recipient {
                address,
                local_account,
                srs_return: None,
                report: None,
                forward_to: None,
                trap: false,
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
            receive(&self.smtp, &self.origin(), envelope, recipients, raw).await
        };
        self.reply(&response).await
    }

    fn origin(&self) -> Origin {
        Origin::Client {
            peer: self.peer,
            helo: self.helo.clone().unwrap_or_default(),
            tls: self.stream.as_ref().and_then(Stream::tls_description),
            submitted: self.account.is_some(),
        }
    }
}

/// Mail from another server, or out of a mailbox elsewhere, for our own people.
pub(crate) async fn receive(
    smtp: &Smtp,
    origin: &Origin,
    envelope: Envelope,
    recipients: Vec<Recipient>,
    raw: Vec<u8>,
) -> String {
    let ctx = smtp.inner.clone();
    let id = random_id();
    let raw = headers::strip_forged_auth_results(&raw, &ctx.hostname);

    let live = ctx.live();
    // Where a fetched message has been, as far as its headers can be trusted.
    let provenance = match origin {
        Origin::Fetched { mailbox, from_junk } => Some(fetched::read(mailbox, *from_junk, &raw)),
        Origin::Client { .. } => None,
    };
    let client = origin.client(&live, &raw, provenance.as_ref());
    if client.is_none()
        && let Origin::Client { peer, .. } = origin
    {
        tracing::warn!(%id, relay = %peer, "no readable Received header from the trusted relay, skipping sender checks");
    }

    let verdict = match (&client, origin) {
        (Some((ip, helo)), _) if live.smtp.verify_senders => {
            Some(checks::verify(&ctx, *ip, helo, &envelope.address, &raw).await)
        }
        // A fetched message the provider vouched for nothing about: its signatures are still
        // worth checking, and they are all that is left to check.
        (None, Origin::Fetched { .. }) if live.smtp.verify_senders => Some(checks::verify_signatures(&ctx, &raw).await),
        _ => None,
    };
    if let Some(checks::Verdict { action: Action::Reject(reason), .. }) = &verdict {
        tracing::info!(%id, from = %envelope.address, %reason, "rejected by DMARC");
        let note = SpamNote {
            id: &id,
            action: SpamAction::Dmarc,
            envelope: &envelope,
            raw: &raw,
            client: client.as_ref(),
            verdict: verdict.as_ref(),
            score: None,
            recipients: all_recipients(&recipients, SpamAction::Dmarc),
            blob_hash: None,
            virus: None,
        };
        note_spam(&ctx, &live.spam.log, note).await;
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
        let note = SpamNote {
            id: &id,
            action: SpamAction::Blocked,
            envelope: &envelope,
            raw: &raw,
            client: client.as_ref(),
            verdict: verdict.as_ref(),
            score: None,
            recipients: all_recipients(&recipients, SpamAction::Blocked),
            blob_hash: None,
            virus: None,
        };
        note_spam(&ctx, &live.spam.log, note).await;
        return "550 5.7.1 Mail from this sender is not accepted here\r\n".into();
    }
    let allowed = decisions.iter().any(|decision| matches!(decision, Decision::Allow(_)));

    // A virus is not a matter of points: a message carrying one is never taken, no matter who
    // sent it or who allowed them. It is also not scored afterwards, so nothing learns from it.
    let checked = clamav::check(&live.spam.antivirus, &raw).await;
    if let clamav::Checked::Found(name) = &checked {
        tracing::info!(%id, from = %envelope.address, virus = %name, "refused, the virus scanner found something");
        let note = SpamNote {
            id: &id,
            action: SpamAction::Virus,
            envelope: &envelope,
            raw: &raw,
            client: client.as_ref(),
            verdict: verdict.as_ref(),
            score: None,
            recipients: all_recipients(&recipients, SpamAction::Virus),
            blob_hash: None,
            virus: Some(name),
        };
        note_spam(&ctx, &live.spam.log, note).await;
        return format!("554 5.7.0 This message contains {name}\r\n");
    }

    // Only mail we can attribute to a sending server is scored; behind a relay that means the
    // server the relay talked to. A fetched message is scored even without one: what is left of
    // it -- the message itself, what it learned from and where the provider says it has been --
    // is still more than nothing.
    let source = spam::Source {
        ip: client.as_ref().map(|(ip, _)| *ip),
        helo: client.as_ref().map_or("", |(_, helo)| helo.as_str()),
        verdict: verdict.as_ref(),
        fetched: provenance.as_ref(),
    };
    let score = if live.spam.enabled && (client.is_some() || provenance.is_some()) {
        spam::score(&ctx, &live.spam, source, &raw).await
    } else {
        None
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
    // A trap keeps the worst of what arrives, so the score never turns it away either. What a virus
    // scanner or DMARC refuses stays refused: no amount of learning material is worth keeping that.
    let trapped = recipients.iter().any(|recipient| recipient.trap);
    // Without a trap this message is refused here; with one it is accepted so the trap keeps
    // collecting, but it must still not be delivered to the real co-recipients (S-17). The delivery
    // loop below skips them when this holds.
    let server_rejects = outcome == spam::Outcome::Reject && !allowed;
    if (server_rejects && !trapped) || everyone_refuses {
        tracing::info!(
            %id,
            from = %envelope.address,
            score = spam_score,
            tests = spam_tests.as_deref(),
            "refused as spam"
        );
        let note = SpamNote {
            id: &id,
            action: SpamAction::Reject,
            envelope: &envelope,
            raw: &raw,
            client: client.as_ref(),
            verdict: verdict.as_ref(),
            score: score.as_ref(),
            recipients: all_recipients(&recipients, SpamAction::Reject),
            blob_hash: None,
            virus: None,
        };
        note_spam(&ctx, &live.spam.log, note).await;
        return "550 5.7.1 This message looks like spam\r\n".into();
    }
    // What a retry of this message hashes to. Taken before our own headers go on top, so the
    // same message from the same server lands on the same value when it comes back.
    let raw_hash = live.spam.greylist_hold.then(|| uwumail_store::BlobHash::of(&raw).as_str().to_owned());

    // Our verdict replaces whatever the message brought along.
    let raw = if score.is_some() { headers::strip_spam_verdicts(&raw) } else { raw };
    let raw = match checked {
        clamav::Checked::Off => raw,
        _ => headers::strip_virus_verdicts(&raw),
    };

    let single = (recipients.len() == 1).then(|| recipients[0].address.as_str());
    let mut message = origin.received_header(&ctx, &id, single).into_bytes();
    if let Some(verdict) = &verdict {
        message.extend_from_slice(verdict.header.as_bytes());
    }
    if let Some(header) = clamav::header(&checked) {
        message.extend_from_slice(header.as_bytes());
    }
    if let Some(score) = &score {
        message.extend_from_slice(spam::headers(score, junk, live.spam.junk_score).as_bytes());
    }
    message.extend_from_slice(&raw);

    // The other half of recognising a returning message, for senders that rewrite something
    // between attempts.
    let message_id = raw_hash.is_some().then(|| headers::first_value(&raw, "Message-ID")).flatten();

    if outcome == spam::Outcome::Suspicious
        && let Some((ip, _)) = &client
    {
        let mut waiting = Vec::new();
        for (recipient, decision) in recipients.iter().zip(&decisions) {
            // An allowed sender never waits, and neither does the message for the others then.
            if matches!(decision, Decision::Allow(_)) {
                continue;
            }
            // Neither does a spam trap. Greylisting works by sending spammers away in the hope that
            // they never come back -- which is exactly what a trap must not do, because then it
            // never sees what it was built to collect.
            if recipient.trap {
                continue;
            }
            let wait = spam::greylist_wait(&ctx, &live.spam, *ip, &envelope.address, &recipient.address).await;
            if let Some(seconds) = wait {
                waiting.push(seconds);
            }
        }
        // An allowed sender lifts greylisting for everyone; otherwise the message is held only
        // while every recipient that can wait still is. A trap never waits and must not, on its
        // own, stop the real recipients from being greylisted (security-audit-0.5.2 S-17). Once one
        // recipient may have it, delivering to all keeps the retry from arriving twice.
        let any_allowed = decisions.iter().any(|decision| matches!(decision, Decision::Allow(_)));
        let eligible = recipients.iter().filter(|recipient| !recipient.trap).count();
        if !any_allowed
            && eligible > 0
            && waiting.len() == eligible
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
            let note = SpamNote {
                id: &id,
                action: SpamAction::Greylist,
                envelope: &envelope,
                raw: &raw,
                client: client.as_ref(),
                verdict: verdict.as_ref(),
                score: score.as_ref(),
                recipients: all_recipients(&recipients, SpamAction::Greylist),
                blob_hash: None,
                virus: None,
            };
            note_spam(&ctx, &live.spam.log, note).await;
            if let Some(raw_hash) = &raw_hash {
                hold_greylisted(
                    &ctx,
                    HeldMessage {
                        id: &id,
                        envelope: &envelope,
                        recipients: &recipients,
                        raw: &raw,
                        message: &message,
                        raw_hash,
                        message_id: message_id.as_deref(),
                        client_ip: ip.to_string(),
                        score: spam_score,
                        subject: score.as_ref().map(|score| score.subject.clone()),
                    },
                )
                .await;
            }
            return format!("451 4.7.1 Please try again in {minutes} minutes\r\n");
        }
    }

    let mut delivered = 0;
    let mut inbox_accounts = Vec::new();
    let mut failed: Vec<FailedRecipient> = Vec::new();
    let mut temporary = false;
    let mut seen_accounts = Vec::new();
    let mut returned = Vec::new();
    // What happened for each of them, for the history. The message as a whole is one decision,
    // but a sender list or someone's own filter can send it two ways at once.
    let mut noted: Vec<SpamLogRecipient> = Vec::new();
    let mut note_for = |address: &str, action: SpamAction, mailbox: Option<&str>| {
        noted.push(SpamLogRecipient {
            address: address.to_owned(),
            action: action.as_str().to_owned(),
            mailbox: mailbox.map(str::to_owned),
        });
    };
    for ((recipient, decision), theirs) in recipients.iter().zip(&decisions).zip(&personal) {
        if let Some(original) = &recipient.srs_return {
            returned.push(NewQueueRecipient { address: original.clone(), notify_flags: 0, orcpt: None });
            continue;
        }
        // A trap address takes the message, teaches the whole server what spam looks like, and
        // delivers it nowhere. The sender hears the same 250 as everyone: a trap that answers
        // differently is crossed off the list and then it catches nothing.
        if recipient.trap {
            if live.spam.bayes
                && let Err(err) = ctx.store.learn_message(&message, true).await
            {
                tracing::warn!(%id, %err, "a trapped message could not be handed to the filter");
            }
            tracing::info!(%id, from = %envelope.address, to = %recipient.address, "caught in a spam trap");
            note_for(&recipient.address, SpamAction::Junk, None);
            delivered += 1;
            continue;
        }
        if let Some(kind) = recipient.report {
            let authenticated = verdict.as_ref().is_some_and(|verdict| verdict.dmarc_passed);
            reports::receive_soon(ctx.clone(), kind, recipient.address.clone(), raw.clone(), authenticated);
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
            note_for(&recipient.address, if junk { SpamAction::Junk } else { SpamAction::Delivered }, None);
            delivered += 1;
            continue;
        }
        let Some(account_id) = recipient.local_account else { continue };
        // A message the server score rejects reaches here only because a trap kept it from being
        // refused outright. It still must not land in a real person's mailbox (security-audit-0.5.2
        // S-17); the trap above already learned from it.
        if server_rejects {
            note_for(&recipient.address, SpamAction::Reject, None);
            continue;
        }
        if seen_accounts.contains(&account_id) {
            continue;
        }
        seen_accounts.push(account_id);
        // The sender came back with a message this person already dealt with by hand while it
        // was waiting. Take it and let it go: delivering it again would double it, and bringing
        // a discarded one back would undo what they decided.
        if let Some(raw_hash) = &raw_hash {
            match ctx.store.returning_greylist_hold(account_id, raw_hash, message_id.as_deref()).await {
                Ok(uwumail_store::Returning::Fresh) => {}
                Ok(settled) => {
                    tracing::info!(
                        %id,
                        account = account_id,
                        discarded = settled == uwumail_store::Returning::Discarded,
                        "a returning greylisted message was already settled by hand"
                    );
                    note_for(&recipient.address, SpamAction::Settled, None);
                    delivered += 1;
                    continue;
                }
                Err(err) => tracing::warn!(%id, account = account_id, %err, "looking up a held message failed"),
            }
        }
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
            note_for(&recipient.address, SpamAction::Delivered, None);
            delivered += 1;
            inbox_accounts.push(account_id);
            continue;
        }
        // What stays is sorted by the person's own rules, if they have some. Junk stays in Junk: the
        // rules are for the mail they want (docs/sieve.md).
        let script = if junk {
            None
        } else {
            ctx.store.active_sieve_script(account_id).await.unwrap_or_else(|err| {
                tracing::warn!(%id, account = account_id, %err, "reading the sieve script failed, delivering to the inbox");
                None
            })
        };
        let stored = match script {
            Some((_, script)) => {
                rules::deliver(&ctx, account_id, script, &recipient.address, &envelope.address, &message)
                    .await
                    .map(|filed| (filed.mailbox, filed.stored))
            }
            None => {
                let mailboxes = vec![MailboxTarget::Role(if junk { MailboxRole::Junk } else { MailboxRole::Inbox })];
                let request =
                    IngestRequest { account_id, raw: message.clone(), mailboxes, keywords: vec![], received_at: None };
                ctx.store.ingest(request).await.map(|_| (Some(if junk { "junk" } else { "inbox" }), true))
            }
        };
        match stored {
            Ok((mailbox, kept)) => {
                note_for(&recipient.address, if junk { SpamAction::Junk } else { SpamAction::Delivered }, mailbox);
                delivered += 1;
                // A message the rules discarded gets no vacation reply either.
                if !junk && kept {
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
    // Every recipient stores the same bytes, so this one name stands for the message wherever it
    // went: the sender's reputation counts it, and the history finds what a person says about it
    // later under the same name.
    let stored = (delivered > 0).then(|| uwumail_store::BlobHash::of(&message));

    // The history of what the filter decided, for the admin page. Mail that reached nobody has
    // its own entry above; this is the one for mail that arrived.
    if delivered > 0 {
        let note = SpamNote {
            id: &id,
            action: if noted.iter().any(|to| to.action == SpamAction::Junk.as_str()) {
                SpamAction::Junk
            } else {
                SpamAction::Delivered
            },
            envelope: &envelope,
            raw: &raw,
            client: client.as_ref(),
            verdict: verdict.as_ref(),
            score: score.as_ref(),
            recipients: noted,
            blob_hash: stored.as_ref().map(|hash| hash.as_str().to_owned()),
            virus: None,
        };
        note_spam(&ctx, &live.spam.log, note).await;
    }

    // Count the message for whoever sent it, so a sender that keeps behaving gets the benefit
    // of the doubt next time, and one that keeps ending up in Junk stops getting it.
    let reputation_subject = spam::reputation_subject(client.as_ref().map(|(ip, _)| *ip), verdict.as_ref());
    if let (Some(score), Some(subject), Some(stored)) = (&score, reputation_subject, &stored) {
        let points = score.points_on_its_own();
        let counts_as_junk =
            quarantined || matches!(spam::outcome(&live.spam, points), spam::Outcome::Junk | spam::Outcome::Reject);
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
            && let Err(err) = ctx.store.queue_bayes_learning(stored.clone(), None, spam).await
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
    // Verified enough that answering it is not backscatter to a forged sender. The same gate an
    // auto-reply and a bounce share (security-audit-0.5.2 S-16).
    let sender_verified = verdict.as_ref().is_none_or(|v| v.sender_verified);
    for account_id in inbox_accounts {
        vacation::maybe_reply(&ctx, account_id, &envelope.address, sender_verified, &message).await;
    }
    if !failed.is_empty() && sender_verified {
        dsn::bounce(&ctx, &envelope.address, &message, &failed).await;
    }
    format!("250 2.0.0 Message accepted as {id}\r\n")
}

impl Session {
    /// Mail from one of our people, to anyone.
    async fn submit(&mut self, envelope: Envelope, recipients: Vec<Recipient>, raw: Vec<u8>) -> String {
        let account = self.account.clone().expect("MAIL requires authentication on submission ports");
        let trace = self.origin().received_header(&self.smtp.inner, &random_id(), None);
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
            Err(SubmitError::SendingOff) => {
                "550 5.7.1 Sending through this server is switched off for this account\r\n".into()
            }
            Err(SubmitError::TooManyRecipients) => "452 4.5.3 Too many recipients\r\n".into(),
            Err(SubmitError::TooLarge) => "552 5.3.4 The message is too large\r\n".into(),
            Err(SubmitError::Virus(name)) => format!("554 5.7.0 This message contains {name}\r\n"),
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

#[cfg(test)]
mod tests {
    use super::mailbox_full;

    /// A full mailbox must never read as a refusal for fetched mail: a refused message is cleared
    /// at the provider, and then a mailbox that ran out of room here would cost somebody their mail
    /// there.
    #[test]
    fn a_full_mailbox_is_recognised_however_it_is_answered() {
        assert!(mailbox_full("552 5.2.2 Mailbox is full"), "how storing it answers");
        assert!(mailbox_full("452 4.2.2 <mini@example.de>: Mailbox is full"), "how RCPT answers");
        assert!(!mailbox_full("550 5.7.1 Message rejected as spam"), "a verdict is not a full mailbox");
        assert!(!mailbox_full("554 5.7.1 DMARC policy of the sender rejects it"));
        assert!(!mailbox_full("550 5.1.1 This address does not take mail"));
        assert!(!mailbox_full("552 5.3.4 Message too big"), "too big is not full");
        assert!(!mailbox_full(""), "and nothing is nothing");
    }
}
