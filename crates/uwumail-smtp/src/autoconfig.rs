//! Setting up a mailbox at another provider from the address alone.
//!
//! Nobody should have to know that iCloud keeps its mail under `imap.mail.me.com`, or that it
//! wants only the part before the `@` as the login while its outgoing server wants the whole
//! address. So this asks, in this order, and stops at the first source that answers:
//!
//! * **What the domain itself says** -- the `_imaps._tcp` and `_submission(s)._tcp` records of
//!   RFC 6186. This is the provider talking about its own mail, so it wins. mail.de and iCloud
//!   both answer here.
//! * **The file the provider publishes for mail programs** -- Thunderbird's autoconfig, at
//!   `autoconfig.<domain>` and under `.well-known` on the domain itself.
//! * **Mozilla's database**, which holds the providers that publish nothing themselves. GMX,
//!   web.de and t-online are only found here.
//! * **Guessing** -- `imap.<domain>` and `mail.<domain>`. Nothing is believed on the strength of
//!   the guess alone: it counts only once the login has really worked.
//!
//! None of it is taken on trust. [`discover`] logs in for real before a mailbox is stored, so what is
//! saved is what a connection actually answered to, not what a database claims. That also settles
//! the one thing no source reliably states -- whether the login is the whole address or only the
//! part before the `@`: when the whole address is refused, the local part is tried once.
//!
//! Everything fetched goes through the same door as subscribed word lists ([`crate::fetch`]):
//! HTTPS with a valid certificate, public addresses only, so a name that points into the local
//! network reaches nothing.

use std::net::SocketAddr;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;

use crate::dnscheck::DnsChecker;
use crate::{Context, Smtp};

/// Mozilla's collection of provider settings, the same one Thunderbird asks.
const DATABASE: &str = "https://autoconfig.thunderbird.net/v1.1/";
const MAX_CONFIG_BYTES: usize = 64 * 1024;
/// A provider that has not greeted us by then is not worth waiting for while somebody watches a
/// spinner.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest line read from an IMAP server while trying it. Greetings and LOGIN answers are short.
const MAX_IMAP_LINE: usize = 8 * 1024;
/// Lines an IMAP server may send in answer to LOGIN before its tagged status.
const MAX_IMAP_LINES: usize = 64;
/// The usual ports, for guessing and for what a source leaves out.
const IMAP_TLS_PORT: u16 = 993;
const SUBMISSION_PORT: u16 = 587;

/// How a connection is encrypted. Never unencrypted: this carries a password that belongs to
/// somebody else's mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// TLS from the first byte, ports 993 and 465.
    Tls,
    /// Plain first, then `STARTTLS`, port 587.
    Starttls,
}

/// What the provider wants as the login name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Login {
    /// `someone@example.com`, which is what most providers want.
    WholeAddress,
    /// `someone` -- iCloud and web.de ask for this on the way in.
    LocalPart,
}

impl Login {
    /// The login name for an address, as this provider spells it.
    pub fn of(self, address: &str) -> String {
        match self {
            Login::WholeAddress => address.to_owned(),
            Login::LocalPart => address.rsplit_once('@').map_or(address, |(local, _)| local).to_owned(),
        }
    }
}

/// One server of a provider, as some source describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub login: Login,
}

/// Who said so. Shown to nobody, but it goes in the log and tells a support question apart from a
/// lucky guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// The domain's own SRV records.
    Domain,
    /// The autoconfig file the provider publishes.
    Provider,
    /// Mozilla's database.
    Database,
    /// `imap.<domain>` and the usual ports.
    Guessed,
    /// The provider's own servers for a sign-in at Microsoft or Google.
    SignIn,
}

/// Everything needed to set a mailbox up, from one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub imap: Server,
    /// Where the provider takes outgoing mail, when a source named one.
    pub smtp: Option<Server>,
    pub source: Source,
}

/// The part after the `@`, lowercased.
fn domain_of(address: &str) -> Option<String> {
    let domain = address.rsplit_once('@')?.1.trim().trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.contains('.') && !domain.contains(char::is_whitespace)).then_some(domain)
}

/// What a source left out: TLS on 993 and 465, STARTTLS on 587, and the whole address as the login
/// until something says otherwise.
fn security_for(port: u16) -> Security {
    match port {
        SUBMISSION_PORT => Security::Starttls,
        _ => Security::Tls,
    }
}

/// What the domain says about its own mail, from `_imaps._tcp` and `_submission(s)._tcp`.
async fn from_domain(dns: &DnsChecker, domain: &str) -> Option<Settings> {
    let imap = dns.service_hosts(&format!("_imaps._tcp.{domain}")).await.into_iter().next()?;
    // A domain may offer both, and implicit TLS is the one that cannot be stripped on the way.
    let implicit = dns.service_hosts(&format!("_submissions._tcp.{domain}")).await.into_iter().next();
    let starttls = dns.service_hosts(&format!("_submission._tcp.{domain}")).await.into_iter().next();
    let smtp = implicit
        .map(|(host, port)| Server { host, port, security: Security::Tls, login: Login::WholeAddress })
        .or_else(|| {
            starttls.map(|(host, port)| Server { host, port, security: security_for(port), login: Login::WholeAddress })
        });
    // `_imaps` is TLS from the first byte by definition, whatever port it names.
    Some(Settings {
        imap: Server { host: imap.0, port: imap.1, security: Security::Tls, login: Login::WholeAddress },
        smtp,
        source: Source::Domain,
    })
}

/// One `<incomingServer>` or `<outgoingServer>` of a client-config file, while it is being read.
#[derive(Default)]
struct PartialServer {
    kind: String,
    host: String,
    port: Option<u16>,
    socket: String,
    login: String,
}

impl PartialServer {
    /// A server this program can use: IMAP or SMTP, encrypted, with a host name. Anything else --
    /// POP3, a plain connection -- is passed over, so a provider that offers both is read as the
    /// one worth having.
    ///
    /// Incoming mail is fetched over TLS from the first byte and nothing else, because that is all
    /// the fetch worker speaks. A provider that publishes only a STARTTLS port for IMAP is passed
    /// over here rather than stored as a mailbox that would fail on every run.
    fn finish(self, want: &str) -> Option<Server> {
        if !self.kind.eq_ignore_ascii_case(want) || self.host.is_empty() {
            return None;
        }
        let security = match self.socket.to_ascii_uppercase().as_str() {
            "SSL" => Security::Tls,
            "STARTTLS" if !want.eq_ignore_ascii_case("imap") => Security::Starttls,
            _ => return None,
        };
        let port = self.port?;
        // `%EMAILLOCALPART%` is how these files say "only the part before the @".
        let login =
            if self.login.to_ascii_uppercase().contains("LOCALPART") { Login::LocalPart } else { Login::WholeAddress };
        Some(Server { host: self.host.to_ascii_lowercase(), port, security, login })
    }
}

/// Reads a Thunderbird client-config file: the first usable IMAP server and the first usable
/// outgoing one. Providers list several (POP3, plain ports) and the order in the file is the
/// order they recommend, so the first that fits is taken.
pub fn read_client_config(xml: &str) -> Option<(Server, Option<Server>)> {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut imap: Option<Server> = None;
    let mut smtp: Option<Server> = None;
    let mut current: Option<PartialServer> = None;
    let mut field = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => {
                let name = tag.name().as_ref().to_owned();
                match name.as_str() {
                    "incomingServer" | "outgoingServer" => {
                        let kind = tag
                            .attributes()
                            .flatten()
                            .find(|attr| attr.key.as_ref() == "type")
                            .map(|attr| attr.value.trim().to_owned())
                            .unwrap_or_default();
                        current = Some(PartialServer { kind, ..PartialServer::default() });
                    }
                    _ => field = name,
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(server) = current.as_mut() {
                    let value = text.trim().to_owned();
                    match field.as_str() {
                        "hostname" => server.host = value,
                        "port" => server.port = value.parse().ok(),
                        "socketType" => server.socket = value,
                        // A file may name several; the first is the one to go by.
                        "username" if server.login.is_empty() => server.login = value,
                        _ => {}
                    }
                }
            }
            Ok(Event::End(tag)) => match tag.name().as_ref() {
                "incomingServer" => {
                    if let Some(found) = current.take().and_then(|server| server.finish("imap")) {
                        imap.get_or_insert(found);
                    }
                }
                "outgoingServer" => {
                    if let Some(found) = current.take().and_then(|server| server.finish("smtp")) {
                        smtp.get_or_insert(found);
                    }
                }
                _ => field.clear(),
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    imap.map(|imap| (imap, smtp))
}

/// Fetches one client-config file. Nothing is believed from a redirect or a body that is not a
/// client config, and the fetcher itself refuses anything but a public address over HTTPS.
async fn config_at(ctx: &Context, url: &str, shown: &str) -> Option<(Server, Option<Server>)> {
    let fetched = ctx.fetcher.get(url, false, None, MAX_CONFIG_BYTES, shown).await.ok()?;
    let crate::fetch::Fetched::Fresh { body, .. } = fetched else {
        return None;
    };
    read_client_config(&String::from_utf8_lossy(&body))
}

/// The two places a provider publishes its own settings, and Mozilla's database after them.
fn config_urls(domain: &str, use_database: bool) -> Vec<String> {
    let mut urls = vec![
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress=user@{domain}"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
    ];
    if use_database {
        urls.push(format!("{DATABASE}{domain}"));
    }
    urls
}

/// What to try when nobody says anything: the names almost every provider uses.
pub fn guesses(domain: &str) -> Vec<Settings> {
    ["imap", "mail"]
        .into_iter()
        .map(|prefix| Settings {
            imap: Server {
                host: format!("{prefix}.{domain}"),
                port: IMAP_TLS_PORT,
                security: Security::Tls,
                login: Login::WholeAddress,
            },
            smtp: Some(Server {
                host: format!("smtp.{domain}"),
                port: SUBMISSION_PORT,
                security: Security::Starttls,
                login: Login::WholeAddress,
            }),
            source: Source::Guessed,
        })
        .collect()
}

/// Everything worth trying for an address, best first. The list is walked by [`discover`], which
/// stops at the first entry a login really works against.
async fn candidates(ctx: &Context, dns: Option<&DnsChecker>, address: &str, use_database: bool) -> Vec<Settings> {
    let Some(domain) = domain_of(address) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if let Some(dns) = dns
        && let Some(settings) = from_domain(dns, &domain).await
    {
        found.push(settings);
    }
    let urls = config_urls(&domain, use_database);
    let last = urls.len() - 1;
    for (index, url) in urls.iter().enumerate() {
        // The last url is Mozilla's, the ones before it are the provider's own.
        let source = if use_database && index == last { Source::Database } else { Source::Provider };
        let shown = if source == Source::Database { "the provider database" } else { "the provider's settings" };
        if let Some((imap, smtp)) = config_at(ctx, url, shown).await {
            found.push(Settings { imap, smtp, source });
            break;
        }
    }
    found.extend(guesses(&domain));
    found
}

/// What came of trying one set of settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The login worked. The settings carry the login the provider accepted.
    Worked(Settings),
    /// The server answered and refused the password. Trying other settings would only lock the
    /// account out, so the search stops here.
    WrongPassword,
    /// Nothing answered, or not in a way this program understands.
    NoAnswer(String),
    /// The provider takes no passwords any more ("Basic authentication is disabled"): only signing
    /// in with OAuth opens this mailbox.
    PasswordsRefused,
}

/// The one address of a host this server may connect to. Only public ones: a mailbox nobody has
/// proven yet must not point this server at itself or into the local network
/// (security-audit-0.5.2 S-10).
async fn public_address(host: &str, port: u16) -> Result<SocketAddr, String> {
    tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| format!("{host} could not be looked up"))?
        .find(|addr| crate::fetch::is_public(addr.ip()))
        .ok_or_else(|| format!("{host} is not a public server"))
}

/// Opens an encrypted connection and reads the greeting, so a wrong host fails here rather than in
/// the middle of a login. The certificate has to be valid for the name that was typed -- this is a
/// password for somebody else's mailbox, not opportunistic MX delivery.
async fn imap_stream(
    ctx: &Context,
    host: &str,
    port: u16,
) -> Result<BufReader<tokio_rustls::client::TlsStream<TcpStream>>, String> {
    let name = rustls_pki_types::ServerName::try_from(host.to_owned()).map_err(|_| "not a server name".to_owned())?;
    let addr = public_address(host, port).await?;
    let tcp = TcpStream::connect(addr).await.map_err(|err| err.to_string())?;
    let tls = tokio_rustls::TlsConnector::from(ctx.client_tls.verified.clone());
    let stream = tls.connect(name, tcp).await.map_err(|err| err.to_string())?;
    let mut reader = BufReader::new(stream);
    let greeting = imap_line(&mut reader, host).await?.unwrap_or_default();
    if !greeting.starts_with("* OK") {
        return Err(format!("{host} did not greet us as an IMAP server"));
    }
    Ok(reader)
}

/// One line from an IMAP server, `None` once it closed the connection. The server is one the
/// address's domain named, so whoever owns the domain decides what it sends: a line is read up to
/// [`MAX_IMAP_LINE`] and no further, instead of until a newline that may never come
/// (security-audit-0.8.0 W-3).
async fn imap_line<R: AsyncBufRead + Unpin>(reader: &mut R, host: &str) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(MAX_IMAP_LINE as u64 + 1)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|err| err.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    if read > MAX_IMAP_LINE {
        return Err(format!("{host} sent a line longer than an IMAP server would"));
    }
    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
}

/// What a server answered to a login.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoginAnswer {
    Accepted,
    /// It said no, in these words.
    Refused(String),
}

/// Reads the answers to `a1 LOGIN` (or `a1 AUTHENTICATE`) up to its tagged one. A continuation --
/// XOAUTH2's error challenge -- is answered with an empty line, after which the server says no.
async fn login_answer<R: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    reader: &mut BufReader<R>,
    host: &str,
) -> Result<LoginAnswer, String> {
    let mut continued = false;
    for _ in 0..MAX_IMAP_LINES {
        let Some(line) = imap_line(reader, host).await? else {
            return Err(format!("{host} broke the connection off during the login"));
        };
        if let Some(rest) = line.strip_prefix("a1 ") {
            let rest = rest.trim_end();
            return Ok(if rest.starts_with("OK") {
                LoginAnswer::Accepted
            } else {
                LoginAnswer::Refused(rest.to_owned())
            });
        }
        if line.starts_with('+') && !continued {
            continued = true;
            reader.get_mut().write_all(b"\r\n").await.map_err(|err| err.to_string())?;
        }
    }
    Err(format!("{host} kept talking instead of answering the login"))
}

/// An IMAP string, with the two characters that need it escaped.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// How to log in: with a password, or with an OAuth access token as SASL XOAUTH2.
#[derive(Clone, Copy)]
enum Credential<'a> {
    Password(&'a str),
    Bearer(&'a str),
}

/// Logs in once and says nothing else.
async fn imap_login(
    ctx: &Context,
    host: &str,
    port: u16,
    user: &str,
    credential: Credential<'_>,
) -> Result<LoginAnswer, String> {
    let mut stream = imap_stream(ctx, host, port).await?;
    let command = match credential {
        Credential::Password(password) => format!("a1 LOGIN {} {}\r\n", quoted(user), quoted(password)),
        Credential::Bearer(token) => {
            format!("a1 AUTHENTICATE XOAUTH2 {}\r\n", crate::provider_oauth::xoauth2(user, token))
        }
    };
    stream.get_mut().write_all(command.as_bytes()).await.map_err(|err| err.to_string())?;
    let answer = login_answer(&mut stream, host).await?;
    let _ = stream.get_mut().write_all(b"a2 LOGOUT\r\n").await;
    Ok(answer)
}

/// Logs in to the provider's outgoing server exactly the way the queue will later: the same
/// client, the same handshake, the same `AUTH PLAIN`. Encrypted before the password goes out, and
/// with a certificate that is valid for the name -- a relay, not an MX.
async fn smtp_login(ctx: &Context, server: &Server, user: &str, credential: Credential<'_>) -> Result<bool, String> {
    let string = |err: std::io::Error| err.to_string();
    let addr = public_address(&server.host, server.port).await?;
    let mut client = crate::client::Client::connect(ctx, addr, PROBE_TIMEOUT, PROBE_TIMEOUT).await.map_err(string)?;
    if server.security == Security::Tls {
        client = client.tls_handshake(ctx.client_tls.verified.clone(), &server.host).await.map_err(string)?;
    }
    let greeting = client.read_reply().await.map_err(string)?;
    if greeting.code != 220 {
        return Err(format!("{} did not greet us: {greeting}", server.host));
    }
    let (reply, caps) = client.ehlo(&ctx.hostname).await.map_err(string)?;
    if !reply.is_positive() {
        return Err(format!("{} refused our greeting: {reply}", server.host));
    }
    if server.security == Security::Starttls {
        if !caps.starttls {
            return Err(format!("{} does not offer STARTTLS", server.host));
        }
        let reply = client.send("STARTTLS\r\n").await.map_err(string)?;
        if reply.code != 220 {
            return Err(format!("{} refused STARTTLS: {reply}", server.host));
        }
        client = client.tls_handshake(ctx.client_tls.verified.clone(), &server.host).await.map_err(string)?;
        let (reply, _) = client.ehlo(&ctx.hostname).await.map_err(string)?;
        if !reply.is_positive() {
            return Err(format!("{} refused our greeting after STARTTLS: {reply}", server.host));
        }
    }
    let reply = match credential {
        Credential::Password(password) => client.auth_plain(user, password).await,
        Credential::Bearer(token) => client.auth_xoauth2(user, token).await,
    }
    .map_err(string)?;
    let accepted = reply.is_positive();
    client.quit().await;
    Ok(accepted)
}

/// Tries one set of settings for real: the whole address as the login, and the part before the `@`
/// after it when the provider refused the first. Two attempts at most, so this never walks into a
/// lockout.
async fn try_settings(ctx: &Context, settings: &Settings, address: &str, password: &str) -> Probe {
    let local = Login::LocalPart.of(address);
    let mut logins = vec![settings.imap.login];
    if local != address && !logins.contains(&Login::LocalPart) {
        logins.push(Login::LocalPart);
    }
    let mut last = String::new();
    for login in logins {
        match imap_login(
            ctx,
            &settings.imap.host,
            settings.imap.port,
            &login.of(address),
            Credential::Password(password),
        )
        .await
        {
            // Microsoft takes no passwords at this mailbox at all. Not a wrong one: nothing else is
            // tried, and the person hears that signing in with Microsoft is the way.
            Ok(LoginAnswer::Refused(text)) if crate::provider_oauth::is_basic_auth_disabled(&text) => {
                return Probe::PasswordsRefused;
            }
            Ok(LoginAnswer::Accepted) => {
                let mut settled = settings.clone();
                settled.imap.login = login;
                return Probe::Worked(settled);
            }
            // The server is there and the password is wrong: asking it again with another spelling
            // is what fills a provider's lockout counter.
            Ok(LoginAnswer::Refused(_)) if login == Login::LocalPart || local == address => {
                return Probe::WrongPassword;
            }
            Ok(LoginAnswer::Refused(_)) => continue,
            Err(err) => last = err,
        }
    }
    Probe::NoAnswer(last)
}

/// Proves the outgoing server before it is offered, with the login the provider takes on the way
/// out -- which is not always the one it takes on the way in. A mailbox that cannot send is better
/// than one that claims it can.
async fn settle_sending(ctx: &Context, settled: &mut Settings, address: &str, password: &str) {
    let Some(smtp) = settled.smtp.clone() else {
        return;
    };
    for login in [smtp.login, Login::WholeAddress, Login::LocalPart] {
        let user = login.of(address);
        if matches!(smtp_login(ctx, &smtp, &user, Credential::Password(password)).await, Ok(true)) {
            settled.smtp.as_mut().expect("the outgoing server is there").login = login;
            return;
        }
    }
    settled.smtp = None;
}

/// What is worth trying for an address, best first, without logging in anywhere and without
/// asking Mozilla's database: a suggestion for a form the admin still checks (a domain move, where
/// nobody's password is at hand yet).
pub async fn suggest(smtp: &Smtp, dns: Option<&DnsChecker>, address: &str) -> Vec<Settings> {
    candidates(&smtp.inner, dns, address, false).await
}

/// Finds out how a provider's mailbox is reached, and proves it by logging in.
///
/// `use_database` asks Mozilla's collection as well, which means telling it the domain.
pub async fn discover(
    smtp: &Smtp,
    dns: Option<&DnsChecker>,
    address: &str,
    password: &str,
    use_database: bool,
) -> Result<Settings, String> {
    let ctx = &smtp.inner;
    let candidates = candidates(ctx, dns, address, use_database).await;
    if candidates.is_empty() {
        return Err("notAnAddress".to_owned());
    }
    let mut last = String::new();
    for settings in candidates {
        let attempt = tokio::time::timeout(PROBE_TIMEOUT, try_settings(ctx, &settings, address, password)).await;
        match attempt {
            Ok(Probe::Worked(mut settled)) => {
                settle_sending(ctx, &mut settled, address, password).await;
                return Ok(settled);
            }
            Ok(Probe::WrongPassword) => return Err("wrongPassword".to_owned()),
            Ok(Probe::PasswordsRefused) => return Err("passwordsRefused".to_owned()),
            Ok(Probe::NoAnswer(err)) => last = err,
            Err(_) => last = "the provider did not answer in time".to_owned(),
        }
    }
    tracing::debug!(%last, "no settings worked for a fetched mailbox");
    Err("notFound".to_owned())
}

/// Proves a sign-in at Microsoft or Google by logging in with its access token, as SASL XOAUTH2, to
/// the provider's own servers, and its outgoing server the same way. The incoming server has to take
/// it -- that is what makes the mailbox this person's; an outgoing server that does not is left out,
/// as with a password. Errors are codes for the portal.
pub async fn prove_sign_in(
    smtp: &Smtp,
    servers: &crate::provider_oauth::ProviderServers,
    address: &str,
    token: &str,
) -> Result<Settings, String> {
    let ctx = &smtp.inner;
    let login = tokio::time::timeout(
        PROBE_TIMEOUT,
        imap_login(ctx, &servers.imap_host, servers.imap_port, address, Credential::Bearer(token)),
    )
    .await;
    match login {
        Ok(Ok(LoginAnswer::Accepted)) => {}
        Ok(Ok(LoginAnswer::Refused(text))) => {
            // Usually a sign-in with another account than the address that was typed.
            tracing::info!(%address, answer = %text, "the provider refused a fresh sign-in");
            return Err("signInRefused".into());
        }
        Ok(Err(err)) => {
            tracing::warn!(%address, %err, "the provider's server could not be reached to prove a sign-in");
            return Err("providerUnreachable".into());
        }
        Err(_) => return Err("providerUnreachable".into()),
    }
    let sending = Server {
        host: servers.smtp_host.clone(),
        port: servers.smtp_port,
        security: if servers.smtp_tls { Security::Tls } else { Security::Starttls },
        login: Login::WholeAddress,
    };
    let sends = matches!(
        tokio::time::timeout(PROBE_TIMEOUT, smtp_login(ctx, &sending, address, Credential::Bearer(token))).await,
        Ok(Ok(true))
    );
    Ok(Settings {
        imap: Server {
            host: servers.imap_host.clone(),
            port: servers.imap_port,
            security: Security::Tls,
            login: Login::WholeAddress,
        },
        smtp: sends.then_some(sending),
        source: Source::SignIn,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection whose server has already said `bytes`, and hears whatever is written to it.
    async fn answering(bytes: &[u8]) -> BufReader<tokio::io::DuplexStream> {
        let (mut server, client) = tokio::io::duplex(bytes.len() + 1024);
        server.write_all(bytes).await.unwrap();
        // Kept open, so what the client writes has somewhere to go.
        tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = server.read_to_end(&mut sink).await;
        });
        BufReader::new(client)
    }

    /// security-audit-0.8.0 W-3: a server that sends a line without an end, or keeps talking instead
    /// of answering the login, is given up on at a limit; a real answer reads as before.
    #[tokio::test]
    async fn an_endless_imap_answer_is_given_up_on() {
        assert_eq!(
            login_answer(
                &mut answering(b"* CAPABILITY IMAP4rev1\r\na1 OK LOGIN completed\r\n").await,
                "imap.example.com"
            )
            .await,
            Ok(LoginAnswer::Accepted)
        );
        assert_eq!(
            login_answer(&mut answering(b"a1 NO [AUTHENTICATIONFAILED] wrong\r\n").await, "imap.example.com").await,
            Ok(LoginAnswer::Refused("NO [AUTHENTICATIONFAILED] wrong".into()))
        );
        // XOAUTH2's error challenge is answered, and the refusal after it read.
        assert_eq!(
            login_answer(
                &mut answering(b"+ eyJzdGF0dXMiOiI0MDEifQ==\r\na1 NO AUTHENTICATE failed.\r\n").await,
                "x.example"
            )
            .await,
            Ok(LoginAnswer::Refused("NO AUTHENTICATE failed.".into()))
        );

        let mut endless = b"* OK ".to_vec();
        endless.resize(1024 * 1024, b'x');
        let error = imap_line(&mut &endless[..], "imap.example.com").await.unwrap_err();
        assert!(error.contains("longer"), "{error}");

        let chatter = "* OK still here\r\n".repeat(MAX_IMAP_LINES + 1) + "a1 OK\r\n";
        let error = login_answer(&mut answering(chatter.as_bytes()).await, "imap.example.com").await.unwrap_err();
        assert!(error.contains("kept talking"), "{error}");
    }

    const ICLOUD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
        <clientConfig version="1.1">
          <emailProvider id="icloud.com">
            <incomingServer type="imap">
              <hostname>imap.mail.me.com</hostname>
              <port>993</port>
              <socketType>SSL</socketType>
              <username>%EMAILLOCALPART%</username>
              <authentication>password-cleartext</authentication>
            </incomingServer>
            <outgoingServer type="smtp">
              <hostname>smtp.mail.me.com</hostname>
              <port>587</port>
              <socketType>STARTTLS</socketType>
              <username>%EMAILADDRESS%</username>
            </outgoingServer>
          </emailProvider>
        </clientConfig>"#;

    /// web.de offers POP3 and a plain port first; the IMAP server with TLS is what we want.
    const WEB_DE: &str = r#"<clientConfig version="1.1">
          <emailProvider id="web.de">
            <incomingServer type="pop3">
              <hostname>pop3.web.de</hostname>
              <port>995</port>
              <socketType>SSL</socketType>
              <username>%EMAILLOCALPART%</username>
            </incomingServer>
            <incomingServer type="imap">
              <hostname>imap.web.de</hostname>
              <port>993</port>
              <socketType>SSL</socketType>
              <username>%EMAILLOCALPART%</username>
            </incomingServer>
            <outgoingServer type="smtp">
              <hostname>smtp.web.de</hostname>
              <port>587</port>
              <socketType>STARTTLS</socketType>
              <username>%EMAILLOCALPART%</username>
            </outgoingServer>
          </emailProvider>
        </clientConfig>"#;

    #[test]
    fn reads_icloud_and_keeps_the_login_apart() {
        let (imap, smtp) = read_client_config(ICLOUD).expect("iCloud has an IMAP server");
        assert_eq!(imap.host, "imap.mail.me.com");
        assert_eq!(imap.port, 993);
        assert_eq!(imap.security, Security::Tls);
        // The one thing a list in the code would get wrong: in on the local part, out on the address.
        assert_eq!(imap.login, Login::LocalPart);
        let smtp = smtp.expect("iCloud has an outgoing server");
        assert_eq!(smtp.host, "smtp.mail.me.com");
        assert_eq!(smtp.port, 587);
        assert_eq!(smtp.security, Security::Starttls);
        assert_eq!(smtp.login, Login::WholeAddress);
    }

    #[test]
    fn passes_over_pop3() {
        let (imap, _) = read_client_config(WEB_DE).expect("web.de has an IMAP server");
        assert_eq!(imap.host, "imap.web.de");
    }

    /// The fetch worker speaks TLS from the first byte only, so a provider that publishes just a
    /// STARTTLS port for IMAP must not become a mailbox that fails on every run -- while its
    /// outgoing server on STARTTLS is perfectly fine.
    #[test]
    fn refuses_starttls_for_fetching_but_not_for_sending() {
        let starttls_imap = ICLOUD.replace(
            "<hostname>imap.mail.me.com</hostname>\n              <port>993</port>\n              <socketType>SSL</socketType>",
            "<hostname>imap.mail.me.com</hostname>\n              <port>143</port>\n              <socketType>STARTTLS</socketType>",
        );
        assert!(read_client_config(&starttls_imap).is_none());
        let (_, smtp) = read_client_config(ICLOUD).expect("iCloud has an IMAP server");
        assert_eq!(smtp.expect("and an outgoing one").security, Security::Starttls);
    }

    #[test]
    fn refuses_a_plain_connection() {
        let plain = ICLOUD.replace("SSL", "plain");
        assert!(read_client_config(&plain).is_none());
    }

    #[test]
    fn nothing_from_nonsense() {
        assert!(read_client_config("not xml at all").is_none());
        assert!(read_client_config("<clientConfig></clientConfig>").is_none());
    }

    #[test]
    fn a_login_is_spelled_both_ways() {
        assert_eq!(Login::WholeAddress.of("someone@icloud.com"), "someone@icloud.com");
        assert_eq!(Login::LocalPart.of("someone@icloud.com"), "someone");
        assert_eq!(Login::LocalPart.of("nonsense"), "nonsense");
    }

    #[test]
    fn a_domain_is_read_out_of_an_address() {
        assert_eq!(domain_of("Someone@Mail.Example.ORG").as_deref(), Some("mail.example.org"));
        assert_eq!(domain_of("someone@icloud.com.").as_deref(), Some("icloud.com"));
        assert_eq!(domain_of("someone@localhost"), None);
        assert_eq!(domain_of("nonsense"), None);
    }

    #[test]
    fn the_database_is_only_asked_when_it_is_allowed() {
        let with = config_urls("mail.example.org", true);
        assert_eq!(with.len(), 3);
        assert!(with[2].starts_with(DATABASE));
        let without = config_urls("mail.example.org", false);
        assert_eq!(without.len(), 2);
        assert!(without.iter().all(|url| !url.contains("thunderbird")));
    }

    #[test]
    fn guessing_uses_the_usual_names() {
        let guesses = guesses("example.com");
        assert_eq!(guesses[0].imap.host, "imap.example.com");
        assert_eq!(guesses[1].imap.host, "mail.example.com");
        assert!(guesses.iter().all(|settings| settings.imap.security == Security::Tls));
    }
}
