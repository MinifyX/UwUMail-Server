//! Sends the server's log lines, and those of its gateway, to a Grafana Loki.
//!
//! Lines wait in a bounded queue and go out in batches through Loki's push API. Loki being away
//! never holds anything up: past the limit the oldest lines are dropped and counted, and the
//! portal says so. The lines are the same JSON the server writes to stdout with
//! `log.format = "json"`, so one set of queries works whichever way they reach Loki.
//!
//! Log lines carry login names and IP addresses. Sending them to another machine is only switched
//! on together with the admin's agreement to exactly that (`privacy_consent`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::Request;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, watch};

use crate::logs::{LogLine, LogSource, level_rank};

/// Lines waiting for Loki. About ten minutes of a busy server; after that the oldest go.
const QUEUE_LIMIT: usize = 10_000;
/// Lines per push. Loki takes far more; smaller pushes keep a retry cheap.
const BATCH_LINES: usize = 1000;
/// How long lines gather before they go out together.
const BATCH_DELAY: Duration = Duration::from_secs(2);
const PUSH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Label names the server sets itself.
const OWN_LABELS: [&str; 4] = ["app", "source", "level", "instance"];

/// What the config file and the admin panel say about sending logs to Loki.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LokiConfig {
    pub enabled: bool,
    /// The admin agreed that log lines, with the login names and IP addresses in them, leave this
    /// server. Without it `enabled` is refused.
    pub privacy_consent: bool,
    /// Loki's address, e.g. `https://logs.example.net`. `/loki/api/v1/push` is added when the
    /// address has no path of its own.
    pub url: String,
    /// Basic authentication, e.g. for Grafana Cloud or a reverse proxy in front of Loki.
    pub username: String,
    pub password: String,
    /// A bearer token instead of a username and password.
    pub token: String,
    /// `X-Scope-OrgID`, for a Loki with more than one tenant.
    pub tenant: String,
    /// Extra labels as `name=value`.
    pub labels: Vec<String>,
    /// The least severe level that is sent: `error`, `warn`, `info` or `debug`. The server's own
    /// `log.level` still comes first: what it does not log cannot be sent.
    pub level: String,
    /// Whether the gateway's lines are sent too.
    pub gateway: bool,
}

impl Default for LokiConfig {
    fn default() -> Self {
        LokiConfig {
            enabled: false,
            privacy_consent: false,
            url: String::new(),
            username: String::new(),
            password: String::new(),
            token: String::new(),
            tenant: String::new(),
            labels: Vec::new(),
            level: "info".into(),
            gateway: true,
        }
    }
}

impl LokiConfig {
    /// Where and how to send, or `None` while switched off. An error says what is wrong, in the
    /// words of the config keys.
    pub fn target(&self, instance: &str) -> Result<Option<LokiTarget>, String> {
        if !self.enabled {
            return Ok(None);
        }
        if !self.privacy_consent {
            return Err("sending logs to Loki needs `log.loki.privacy_consent`: log lines contain login names and \
                        IP addresses"
                .into());
        }
        self.connection(instance).map(Some)
    }

    /// Where and how to send, switched on or not: for a test line before anything is switched on.
    pub fn connection(&self, instance: &str) -> Result<LokiTarget, String> {
        let url = push_url(&self.url)?;
        let auth = match (self.username.trim(), self.password.as_str(), self.token.trim()) {
            ("", "", "") => None,
            (_, _, token) if !token.is_empty() && !self.username.trim().is_empty() => {
                return Err("`log.loki` takes either a username and password or a token, not both".into());
            }
            ("", "", token) => Some(format!("Bearer {token}")),
            ("", _, _) => return Err("`log.loki.password` needs `log.loki.username`".into()),
            (username, password, _) => {
                Some(format!("Basic {}", data_encoding::BASE64.encode(format!("{username}:{password}").as_bytes())))
            }
        };
        let auth = auth
            .map(|value| HeaderValue::from_str(&value).map_err(|_| "the Loki credentials contain odd characters"))
            .transpose()?;
        let tenant = match self.tenant.trim() {
            "" => None,
            tenant => Some(HeaderValue::from_str(tenant).map_err(|_| "`log.loki.tenant` contains odd characters")?),
        };
        let mut labels = Vec::new();
        for entry in &self.labels {
            let (name, value) = entry
                .split_once('=')
                .map(|(name, value)| (name.trim(), value.trim()))
                .ok_or_else(|| format!("the Loki label `{entry}` is not name=value"))?;
            if !valid_label(name) || value.is_empty() {
                return Err(format!("the Loki label `{entry}` is not name=value with a plain name"));
            }
            if OWN_LABELS.contains(&name) {
                return Err(format!("the Loki label `{name}` is set by the server itself"));
            }
            labels.push((name.to_owned(), value.to_owned()));
        }
        let max_rank = match self.level.as_str() {
            "error" | "warn" | "info" | "debug" => level_rank(&self.level),
            other => return Err(format!("`log.loki.level` '{other}' is not error, warn, info or debug")),
        };
        Ok(LokiTarget { url, auth, tenant, labels, max_rank, gateway: self.gateway, instance: instance.to_owned() })
    }
}

fn valid_label(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with("__")
}

/// The push address for what someone typed: the base address of a Loki, or already the whole path.
fn push_url(typed: &str) -> Result<String, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("sending logs to Loki needs `log.loki.url`".into());
    }
    let uri: hyper::Uri = typed.parse().map_err(|_| format!("`log.loki.url` '{typed}' is not an address"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.host().is_none() {
        return Err(format!("`log.loki.url` '{typed}' has to start with http:// or https://"));
    }
    if uri.query().is_some() {
        return Err("`log.loki.url` must not have a query".into());
    }
    let base = typed.trim_end_matches('/');
    Ok(if uri.path().trim_end_matches('/').is_empty() { format!("{base}/loki/api/v1/push") } else { base.to_owned() })
}

/// Where the lines go, checked and ready.
#[derive(Clone, PartialEq, Eq)]
pub struct LokiTarget {
    url: String,
    auth: Option<HeaderValue>,
    tenant: Option<HeaderValue>,
    labels: Vec<(String, String)>,
    max_rank: u8,
    gateway: bool,
    instance: String,
}

impl std::fmt::Debug for LokiTarget {
    // Never the credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LokiTarget").field("url", &self.url).field("instance", &self.instance).finish_non_exhaustive()
    }
}

impl LokiTarget {
    fn wants(&self, line: &LogLine) -> bool {
        level_rank(line.level) <= self.max_rank && (line.source == LogSource::Server || self.gateway)
    }
}

/// How sending goes, for the portal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LokiStatus {
    pub enabled: bool,
    /// Lines waiting to go out.
    pub queued: usize,
    /// Lines Loki took since the server started.
    pub sent: u64,
    /// Lines dropped because Loki was away for too long, or refused them.
    pub dropped: u64,
    /// Unix time of the last push Loki took.
    pub last_success: Option<i64>,
    /// What went wrong with the last push, while it is still going wrong.
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    target: Option<Arc<LokiTarget>>,
    queue: VecDeque<LogLine>,
    status: LokiStatus,
}

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

pub struct Loki {
    state: Mutex<State>,
    arrived: Notify,
    client: HttpClient,
}

impl Loki {
    pub fn new() -> Arc<Loki> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(Duration::from_secs(5)));
        // A Loki in the same network often speaks plain HTTP; the admin typed the address.
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        Arc::new(Loki {
            state: Mutex::new(State::default()),
            arrived: Notify::new(),
            client: Client::builder(TokioExecutor::new()).build(connector),
        })
    }

    /// Switches sending on, over to another Loki, or off. Off forgets what was waiting.
    pub fn set_target(&self, target: Option<LokiTarget>) {
        let mut state = self.state.lock().expect("loki poisoned");
        // Saving some other setting applies all of them again; that must not touch what is on its way.
        if state.target.as_deref() == target.as_ref() {
            return;
        }
        state.status.enabled = target.is_some();
        state.status.error = None;
        if target.is_none() {
            state.queue.clear();
        }
        state.target = target.map(Arc::new);
    }

    pub fn status(&self) -> LokiStatus {
        let state = self.state.lock().expect("loki poisoned");
        LokiStatus { queued: state.queue.len(), ..state.status.clone() }
    }

    /// Takes a copy of a line, if it is one to send. Never waits.
    pub fn offer(&self, line: &LogLine) {
        // What this module says about Loki being away must not pile up in front of Loki.
        if line.target.starts_with(module_path!()) {
            return;
        }
        let mut state = self.state.lock().expect("loki poisoned");
        if !state.target.as_ref().is_some_and(|target| target.wants(line)) {
            return;
        }
        if state.queue.len() == QUEUE_LIMIT {
            state.queue.pop_front();
            state.status.dropped += 1;
        }
        state.queue.push_back(line.clone());
        drop(state);
        self.arrived.notify_one();
    }

    /// Sends one line to `target` right away, to try the address and the credentials.
    pub async fn test(&self, target: &LokiTarget) -> Result<(), String> {
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default();
        let line = LogLine {
            seq: 0,
            at,
            source: LogSource::Server,
            level: "info",
            target: "uwumail".into(),
            message: "a test line from the UwUMail admin panel".into(),
            fields: Vec::new(),
        };
        self.push(target, &[line]).await.map_err(|failure| failure.message)
    }

    /// Sends what waits until `shutdown`, then tries once more to get the rest out.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            tokio::select! {
                _ = self.arrived.notified() => {}
                _ = shutdown.changed() => break,
            }
            tokio::select! {
                _ = tokio::time::sleep(BATCH_DELAY) => {}
                _ = shutdown.changed() => break,
            }
            loop {
                match self.send_batch().await {
                    Sent::Nothing => {
                        backoff = Duration::from_secs(1);
                        break;
                    }
                    Sent::Some => backoff = Duration::from_secs(1),
                    Sent::Failed => {
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = shutdown.changed() => return self.flush().await,
                        }
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
        }
        self.flush().await;
    }

    async fn flush(&self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while matches!(self.send_batch().await, Sent::Some) {}
        })
        .await;
    }

    async fn send_batch(&self) -> Sent {
        let (target, batch) = {
            let mut state = self.state.lock().expect("loki poisoned");
            let Some(target) = state.target.clone() else { return Sent::Nothing };
            let take = state.queue.len().min(BATCH_LINES);
            if take == 0 {
                return Sent::Nothing;
            }
            (target, state.queue.drain(..take).collect::<Vec<_>>())
        };
        let result = self.push(&target, &batch).await;
        let mut state = self.state.lock().expect("loki poisoned");
        // Switched over or off while this was on its way: what it did no longer matters.
        if !state.target.as_ref().is_some_and(|current| Arc::ptr_eq(current, &target)) {
            return Sent::Some;
        }
        match result {
            Ok(()) => {
                state.status.sent += batch.len() as u64;
                state.status.last_success = Some(crate::health::unix_now());
                if state.status.error.take().is_some() {
                    drop(state);
                    tracing::info!("Loki takes the log lines again");
                }
                Sent::Some
            }
            Err(failure) => {
                let first = state.status.error.is_none();
                state.status.error = Some(failure.message.clone());
                if failure.retry {
                    // Back in front, as far as the newer lines leave room.
                    for line in batch.into_iter().rev() {
                        if state.queue.len() == QUEUE_LIMIT {
                            state.status.dropped += 1;
                        } else {
                            state.queue.push_front(line);
                        }
                    }
                } else {
                    state.status.dropped += batch.len() as u64;
                }
                drop(state);
                if first {
                    tracing::warn!(error = %failure.message, "Loki does not take the log lines");
                }
                if failure.retry { Sent::Failed } else { Sent::Some }
            }
        }
    }

    async fn push(&self, target: &LokiTarget, lines: &[LogLine]) -> Result<(), Failure> {
        let body = push_body(target, lines);
        let mut request = Request::post(&target.url)
            .header(CONTENT_TYPE, "application/json")
            .header("User-Agent", concat!("UwUMail/", env!("CARGO_PKG_VERSION")));
        if let Some(auth) = &target.auth {
            request = request.header(AUTHORIZATION, auth.clone());
        }
        if let Some(tenant) = &target.tenant {
            request = request.header("X-Scope-OrgID", tenant.clone());
        }
        let request = request
            .body(Full::new(Bytes::from(body)))
            .map_err(|err| Failure { message: err.to_string(), retry: false })?;
        let sending = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|err| Failure { message: error_chain(&err), retry: true })?;
            let status = response.status();
            if status.is_success() {
                return Ok(());
            }
            let text = Limited::new(response.into_body(), 1024)
                .collect()
                .await
                .map(|body| String::from_utf8_lossy(&body.to_bytes()).trim().to_owned())
                .unwrap_or_default();
            let message = if text.is_empty() {
                format!("Loki answered {status}")
            } else {
                format!("Loki answered {status}: {text}")
            };
            // Too many, or Loki itself in trouble: later. Anything else would be refused again.
            let retry = status == hyper::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            Err(Failure { message, retry })
        };
        tokio::time::timeout(PUSH_TIMEOUT, sending)
            .await
            .map_err(|_| Failure { message: "Loki did not answer in time".into(), retry: true })?
    }
}

enum Sent {
    Nothing,
    Some,
    Failed,
}

struct Failure {
    message: String,
    retry: bool,
}

/// One stream per source and level, each line as the JSON the server writes to stdout.
fn push_body(target: &LokiTarget, lines: &[LogLine]) -> Vec<u8> {
    let mut streams: BTreeMap<(&'static str, &'static str), Vec<Value>> = BTreeMap::new();
    for line in lines {
        let mut fields = Map::new();
        fields.insert("message".into(), Value::String(line.message.clone()));
        for (key, value) in &line.fields {
            fields.insert(key.clone(), Value::String(value.clone()));
        }
        let text = json!({ "level": line.level.to_ascii_uppercase(), "fields": fields }).to_string();
        let nanos = u128::from(line.at) * 1_000_000;
        streams.entry((line.source.as_str(), line.level)).or_default().push(json!([nanos.to_string(), text]));
    }
    let streams: Vec<Value> = streams
        .into_iter()
        .map(|((source, level), values)| {
            let mut labels = Map::new();
            labels.insert("app".into(), json!("uwumail"));
            labels.insert("instance".into(), json!(target.instance));
            labels.insert("source".into(), json!(source));
            labels.insert("level".into(), json!(level));
            for (name, value) in &target.labels {
                labels.insert(name.clone(), json!(value));
            }
            json!({ "stream": labels, "values": values })
        })
        .collect();
    json!({ "streams": streams }).to_string().into_bytes()
}

/// hyper's errors hide the interesting part (a refused connection, a certificate) in their sources.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = inner.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LokiConfig {
        LokiConfig {
            enabled: true,
            privacy_consent: true,
            url: "http://loki.example.net:3100".into(),
            ..LokiConfig::default()
        }
    }

    fn line(source: LogSource, level: &'static str, message: &str) -> LogLine {
        LogLine {
            seq: 0,
            at: 1_700_000_000_123,
            source,
            level,
            target: "uwumail_smtp".into(),
            message: message.into(),
            fields: vec![("login".into(), "leni@example.de".into())],
        }
    }

    #[test]
    fn switching_on_needs_the_privacy_consent() {
        assert!(LokiConfig::default().target("mail.example.de").unwrap().is_none(), "off by default");
        let without = LokiConfig { privacy_consent: false, ..config() };
        assert!(without.target("mail.example.de").unwrap_err().contains("privacy_consent"));
        assert!(without.connection("mail.example.de").is_ok(), "a test line needs no consent");
        assert!(config().target("mail.example.de").unwrap().is_some());
    }

    #[test]
    fn addresses_get_the_push_path_when_they_have_none() {
        assert_eq!(push_url("http://loki:3100").unwrap(), "http://loki:3100/loki/api/v1/push");
        assert_eq!(push_url("https://logs.example.net/").unwrap(), "https://logs.example.net/loki/api/v1/push");
        assert_eq!(
            push_url("https://proxy.example.net/loki/api/v1/push").unwrap(),
            "https://proxy.example.net/loki/api/v1/push"
        );
        assert!(push_url("").is_err());
        assert!(push_url("loki:3100").is_err());
        assert!(push_url("ftp://loki.example.net").is_err());
        assert!(push_url("https://loki.example.net/?x=1").is_err());
    }

    #[test]
    fn credentials_and_labels_are_checked() {
        let basic = LokiConfig { username: "123".into(), password: "secret".into(), ..config() };
        let target = basic.connection("m").unwrap();
        assert_eq!(target.auth.unwrap(), "Basic MTIzOnNlY3JldA==");
        assert!(!format!("{:?}", basic.connection("m").unwrap()).contains("MTIz"), "never in a log");
        let bearer = LokiConfig { token: "t0ken".into(), ..config() };
        assert_eq!(bearer.connection("m").unwrap().auth.unwrap(), "Bearer t0ken");
        assert!(LokiConfig { username: "a".into(), token: "t".into(), ..config() }.connection("m").is_err());
        assert!(LokiConfig { password: "p".into(), ..config() }.connection("m").is_err());

        let labels = LokiConfig { labels: vec!["env=production".into(), " site = home ".into()], ..config() };
        assert_eq!(
            labels.connection("m").unwrap().labels,
            vec![("env".to_owned(), "production".to_owned()), ("site".to_owned(), "home".to_owned())]
        );
        for bad in ["env", "=x", "1env=x", "env=", "__name__=x", "level=x", "app=x"] {
            assert!(LokiConfig { labels: vec![bad.into()], ..config() }.connection("m").is_err(), "{bad}");
        }
        assert!(LokiConfig { level: "trace".into(), ..config() }.connection("m").is_err());
    }

    #[test]
    fn only_wanted_lines_wait_and_the_queue_stays_bounded() {
        let loki = Loki::new();
        loki.offer(&line(LogSource::Server, "info", "before anything was switched on"));
        assert_eq!(loki.status().queued, 0);

        let target = LokiConfig { level: "warn".into(), gateway: false, ..config() };
        loki.set_target(target.target("mail.example.de").unwrap());
        loki.offer(&line(LogSource::Server, "info", "too chatty"));
        loki.offer(&line(LogSource::Server, "warn", "failed imap login"));
        loki.offer(&line(LogSource::Gateway, "error", "not asked for"));
        let mut own = line(LogSource::Server, "warn", "Loki does not take the log lines");
        own.target = module_path!().into();
        loki.offer(&own);
        assert_eq!(loki.status().queued, 1);

        for _ in 0..QUEUE_LIMIT {
            loki.offer(&line(LogSource::Server, "error", "a lot"));
        }
        let status = loki.status();
        assert_eq!((status.queued, status.dropped), (QUEUE_LIMIT, 1));

        loki.set_target(None);
        assert_eq!(loki.status(), LokiStatus { dropped: 1, ..LokiStatus::default() }, "off forgets the queue");
    }

    #[test]
    fn lines_go_out_as_the_servers_json_with_labels() {
        let target =
            LokiConfig { labels: vec!["env=production".into()], ..config() }.connection("mail.example.de").unwrap();
        let body: Value = serde_json::from_slice(&push_body(
            &target,
            &[
                line(LogSource::Server, "warn", "failed imap login"),
                line(LogSource::Gateway, "info", "the UwUMail server is connected"),
                line(LogSource::Server, "warn", "failed smtp login"),
            ],
        ))
        .unwrap();
        let streams = body["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 2, "one stream per source and level");
        let gateway = &streams[0];
        assert_eq!(
            gateway["stream"],
            json!({
                "app": "uwumail", "instance": "mail.example.de", "source": "gateway", "level": "info", "env": "production"
            })
        );
        let server = &streams[1];
        assert_eq!(server["values"].as_array().unwrap().len(), 2);
        assert_eq!(server["values"][0][0], "1700000000123000000");
        let text: Value = serde_json::from_str(server["values"][0][1].as_str().unwrap()).unwrap();
        assert_eq!(
            text,
            json!({ "level": "WARN", "fields": { "message": "failed imap login", "login": "leni@example.de" } })
        );
    }

    /// A tiny Loki on this machine: answers each push with the next status and keeps the bodies.
    async fn fake_loki(answers: Vec<u16>) -> (String, tokio::sync::mpsc::UnboundedReceiver<(String, Value)>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (seen, received) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for answer in answers {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            let _ = seen.send((head.to_owned(), serde_json::from_str(body).unwrap_or(Value::Null)));
                            break;
                        }
                    }
                }
                let reply = format!("HTTP/1.1 {answer} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), received)
    }

    #[tokio::test]
    async fn pushes_retry_while_loki_is_in_trouble() {
        let (url, mut received) = fake_loki(vec![503, 204]).await;
        let loki = Loki::new();
        let config = LokiConfig { url, username: "u".into(), password: "p".into(), tenant: "home".into(), ..config() };
        loki.set_target(config.target("mail.example.de").unwrap());
        let (_stop, stop) = watch::channel(false);
        tokio::spawn(loki.clone().run(stop));
        loki.offer(&line(LogSource::Server, "warn", "failed imap login"));

        let (head, _) = received.recv().await.unwrap();
        assert!(head.starts_with("POST /loki/api/v1/push "));
        let head = head.to_ascii_lowercase();
        assert!(head.contains("authorization: basic dtpw"));
        assert!(head.contains("x-scope-orgid: home"));
        let (_, body) = tokio::time::timeout(Duration::from_secs(10), received.recv()).await.unwrap().unwrap();
        assert_eq!(body["streams"][0]["stream"]["level"], "warn", "the same line again");
        tokio::time::timeout(Duration::from_secs(5), async {
            while loki.status().sent == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let status = loki.status();
        assert_eq!((status.sent, status.queued, status.dropped, status.error), (1, 0, 0, None));
    }

    #[tokio::test]
    async fn a_refused_push_is_dropped_and_the_test_says_why() {
        let (url, _received) = fake_loki(vec![401, 401]).await;
        let loki = Loki::new();
        let config = LokiConfig { url, ..config() };
        let error = loki.test(&config.connection("m").unwrap()).await.unwrap_err();
        assert!(error.contains("401"), "{error}");

        loki.set_target(config.target("m").unwrap());
        loki.offer(&line(LogSource::Server, "warn", "failed imap login"));
        assert!(matches!(loki.send_batch().await, Sent::Some), "not retried");
        let status = loki.status();
        assert_eq!((status.dropped, status.queued), (1, 0));
        assert!(status.error.unwrap().contains("401"));
    }
}
