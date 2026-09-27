//! Statistics, admin alerts and the Prometheus metrics, end to end through the router.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{MailboxRole, NewAccount, Role, Stat, Store};
use uwumail_web::{CSRF_HEADER, CertificateStatus, MetricsConfig, MetricsGate, Web, WebSettings};

struct Server {
    web: Web,
    app: Router,
    store: Store,
    /// Days the certificate has left; starts with ten.
    certificate_days: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, role) in [("nyu@example.org", Role::Admin), ("leni@example.org", Role::User)] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: address.split('@').next().unwrap().into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let certificate_days = Arc::new(AtomicI64::new(10));
    let days = certificate_days.clone();
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.org".into(),
            smtp: Default::default(),
            spam: Default::default(),
            delivery: Default::default(),
            tone: Default::default(),
            server_tls: None,
        },
    )
    .unwrap();
    let web = Web::new(
        smtp,
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            // Ten days left of a Let's Encrypt certificate: it should have been renewed, a warning.
            certificate: Some(Arc::new(move || {
                Some(CertificateStatus {
                    not_after: unix_now() + days.load(Ordering::SeqCst) * 86_400 + 3600,
                    names: vec!["mail.example.org".into()],
                    self_signed: false,
                    automatic: true,
                    lets_encrypt_account: None,
                    chain: Vec::new(),
                })
            })),
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    Server { app: web.router(), web, store, certificate_days, _dir: dir }
}

async fn send(app: &Router, request: Request<Body>, ip: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = request;
    let ip: IpAddr = ip.parse().unwrap();
    request.extensions_mut().insert(ClientInfo { ip, https: true });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn login(app: &Router, login: &str) -> (String, String) {
    let request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": login, "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    let mut request = request;
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

async fn api(app: &Router, method: &str, path: &str, session: &(String, String)) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &session.0)
        .header(CSRF_HEADER, &session.1)
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(app, request, "127.0.0.1").await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn inbox_subjects(store: &Store, login: &str) -> Vec<String> {
    let account = store.account(login).await.unwrap().unwrap();
    let inbox = store.mailboxes(account.id).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
    let emails = store.emails_in_mailbox(inbox.unwrap().id, 50).await.unwrap();
    emails.into_iter().map(|email| email.subject).collect()
}

#[tokio::test]
async fn statistics_show_what_was_counted() {
    let server = server().await;
    server.store.stats().add(Stat::Received, 3);
    server.store.stats().count(Stat::RefusedSpam);
    server.store.flush_stats().await.unwrap();
    // Counted but not written down yet still shows.
    server.store.stats().count(Stat::Received);

    let admin = login(&server.app, "nyu@example.org").await;
    let (status, body) = api(&server.app, "GET", "/api/admin/stats", &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["range"], "days");
    let periods = body["periods"].as_array().unwrap();
    assert_eq!(periods.len(), 30);
    let today = periods.last().unwrap();
    assert_eq!(today["values"]["mail.received"], 4);
    assert_eq!(today["values"]["gauge.accounts"], 2);
    assert_eq!(body["totals"]["mail.received"], 4);
    assert_eq!(body["totals"]["refused.spam"], 1);
    assert!(body["totals"].get("gauge.accounts").is_none(), "readings are not summed up");

    let (status, body) = api(&server.app, "GET", "/api/admin/stats?range=months", &admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["periods"].as_array().unwrap().len(), 12);
    assert_eq!(body["periods"][11]["values"]["mail.received"], 4);
    let (status, _) = api(&server.app, "GET", "/api/admin/stats?range=weeks", &admin).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let person = login(&server.app, "leni@example.org").await;
    let (status, _) = api(&server.app, "GET", "/api/admin/stats", &person).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn alerts_are_listed_mailed_and_acknowledged() {
    let server = server().await;
    let admin = login(&server.app, "nyu@example.org").await;
    let (status, body) = api(&server.app, "GET", "/api/admin/alerts", &admin).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "open": [], "resolved": [] }));

    server.web.check_alerts().await;
    let (_, body) = api(&server.app, "GET", "/api/admin/alerts", &admin).await;
    let open = body["open"].as_array().unwrap();
    let certificate = open.iter().find(|alert| alert["code"] == "certExpiresSoon").expect("{body}");
    assert_eq!(certificate["kind"], "certificate");
    assert_eq!(certificate["level"], "warning");
    // The admin gets one mail about all of it, the person none.
    let subjects = inbox_subjects(&server.store, "nyu@example.org").await;
    assert_eq!(subjects.len(), 1, "{subjects:?}");
    assert!(subjects[0].contains("mail.example.org"), "{subjects:?}");
    assert!(inbox_subjects(&server.store, "leni@example.org").await.is_empty());
    // Nothing new: no second mail.
    server.web.check_alerts().await;
    assert_eq!(inbox_subjects(&server.store, "nyu@example.org").await.len(), 1);

    let path = format!("/api/admin/alerts/{}/acknowledge", certificate["id"]);
    let person = login(&server.app, "leni@example.org").await;
    let (status, _) = api(&server.app, "POST", &path, &person).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = api(&server.app, "POST", &path, &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["acknowledgedBy"], "nyu@example.org");
    let (status, _) = api(&server.app, "POST", "/api/admin/alerts/999/acknowledge", &admin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let log = server.store.audit_log(10, None).await.unwrap();
    assert!(log.iter().any(|entry| entry.action == "alert.acknowledge"));
}

#[tokio::test]
async fn alert_mails_follow_the_admins_choice() {
    let server = server().await;
    let admin = login(&server.app, "nyu@example.org").await;
    let request = Request::patch("/api/account/preferences")
        .header(header::COOKIE, &admin.0)
        .header(CSRF_HEADER, &admin.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "adminAlerts": "problems", "adminView": "simple" }).to_string()))
        .unwrap();
    let (status, _, body) = send(&server.app, request, "127.0.0.1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The certificate is only a warning: nothing for someone who asked for problems only (unless
    // the disk this test runs on is really full, which would be a problem).
    server.web.check_alerts().await;
    let (_, body) = api(&server.app, "GET", "/api/admin/alerts", &admin).await;
    let open = body["open"].as_array().unwrap();
    assert!(open.iter().any(|alert| alert["code"] == "certExpiresSoon"), "the list has it all the same");
    let problems = open.iter().filter(|alert| alert["level"] == "problem").count();
    assert_eq!(inbox_subjects(&server.store, "nyu@example.org").await.len(), usize::from(problems > 0));
}

#[tokio::test]
async fn admins_hear_once_when_it_is_fine_again() {
    let server = server().await;
    let admin = login(&server.app, "nyu@example.org").await;
    let request = Request::patch("/api/account/preferences")
        .header(header::COOKIE, &admin.0)
        .header(CSRF_HEADER, &admin.1)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "language": "en", "tone": "neutral" }).to_string()))
        .unwrap();
    assert_eq!(send(&server.app, request, "127.0.0.1").await.0, StatusCode::OK);

    let start = unix_now();
    server.web.check_alerts_at(start).await;
    let subjects = inbox_subjects(&server.store, "nyu@example.org").await;
    assert_eq!(subjects.len(), 1, "{subjects:?}");
    assert!(!subjects[0].contains("fine again"), "{subjects:?}");

    // Renewed. A moment later is too early to call it fine: it may come straight back.
    server.certificate_days.store(80, Ordering::SeqCst);
    server.web.check_alerts_at(start + 5 * 60).await;
    assert_eq!(inbox_subjects(&server.store, "nyu@example.org").await.len(), 1);
    server.web.check_alerts_at(start + 20 * 60).await;
    let subjects = inbox_subjects(&server.store, "nyu@example.org").await;
    assert_eq!(subjects.len(), 2, "{subjects:?}");
    assert!(subjects.contains(&"mail.example.org is fine again".to_owned()), "{subjects:?}");
    let (_, body) = api(&server.app, "GET", "/api/admin/alerts", &admin).await;
    let resolved = body["resolved"].as_array().unwrap();
    assert!(resolved.iter().any(|alert| alert["code"] == "certExpiresSoon"), "{body}");
    assert!(!body["open"].as_array().unwrap().iter().any(|alert| alert["code"] == "certExpiresSoon"));

    // And only once.
    server.web.check_alerts_at(start + 40 * 60).await;
    assert_eq!(inbox_subjects(&server.store, "nyu@example.org").await.len(), 2);
}

/// Checks the text exposition format line by line and returns the samples by name and labels.
fn parse_metrics(text: &str) -> Vec<(String, String, f64)> {
    let mut described: HashSet<String> = HashSet::new();
    let mut typed: HashSet<String> = HashSet::new();
    let mut samples = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, help) = rest.split_once(' ').unwrap();
            assert!(!help.is_empty());
            assert!(described.insert(name.to_owned()), "HELP twice for {name}");
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap();
            assert!(["gauge", "counter"].contains(&kind), "{line}");
            assert_eq!(kind == "counter", name.ends_with("_total"), "{line}");
            assert!(described.contains(name), "HELP before TYPE: {line}");
            typed.insert(name.to_owned());
            continue;
        }
        assert!(!line.starts_with('#') && !line.is_empty(), "unexpected line {line:?}");
        let (series, value) = line.rsplit_once(' ').unwrap();
        let value: f64 = value.parse().unwrap_or_else(|_| panic!("not a number: {line}"));
        let (name, labels) = match series.split_once('{') {
            Some((name, labels)) => {
                let labels = labels.strip_suffix('}').expect("labels are closed");
                for pair in labels.split(',') {
                    let (label, quoted) = pair.split_once('=').unwrap();
                    assert!(label.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{line}");
                    assert!(quoted.starts_with('"') && quoted.ends_with('"'), "{line}");
                }
                (name, labels)
            }
            None => (series, ""),
        };
        assert!(name.starts_with("uwumail_") && name.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{name}");
        assert!(typed.contains(name), "TYPE before the samples of {name}");
        samples.push((name.to_owned(), labels.to_owned(), value));
    }
    samples
}

fn value(samples: &[(String, String, f64)], name: &str, labels: &str) -> f64 {
    samples.iter().find(|(n, l, _)| n == name && l == labels).unwrap_or_else(|| panic!("no {name}{{{labels}}}")).2
}

#[tokio::test]
async fn metrics_are_off_until_switched_on_and_need_a_token_or_a_network() {
    let server = server().await;
    let scrape = |token: Option<&str>| {
        let mut request = Request::get("/metrics");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        request.body(Body::empty()).unwrap()
    };
    // Not plugged in, and plugged in but off: there is nothing here.
    let (status, _, _) = send(&server.app, scrape(None), "192.0.2.10").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let gate = Arc::new(MetricsGate::default());
    server.web.set_metrics_gate(gate.clone());
    let (status, _, _) = send(&server.app, scrape(None), "192.0.2.10").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A token, from anywhere.
    let token = "scrape-me-gently-0123456789";
    gate.configure(&MetricsConfig { enabled: true, token: token.into(), allowed_networks: vec![] }).unwrap();
    let (status, headers, _) = send(&server.app, scrape(None), "192.0.2.10").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    let (status, _, _) = send(&server.app, scrape(Some("wrong-token-wrong-token")), "192.0.2.10").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    server.store.stats().add(Stat::Received, 2);
    server.store.stats().count(Stat::RefusedVirus);
    server.store.stats().count(Stat::login_failed("imap"));
    let (status, headers, text) = send(&server.app, scrape(Some(token)), "192.0.2.10").await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/plain; version=0.0.4"));
    let samples = parse_metrics(&text);
    assert_eq!(value(&samples, "uwumail_build_info", &format!("version=\"{}\"", env!("CARGO_PKG_VERSION"))), 1.0);
    assert_eq!(value(&samples, "uwumail_accounts", ""), 2.0);
    assert_eq!(value(&samples, "uwumail_domains", ""), 1.0);
    assert_eq!(value(&samples, "uwumail_mail_received_total", ""), 2.0);
    assert_eq!(value(&samples, "uwumail_mail_refused_total", "reason=\"virus\""), 1.0);
    assert_eq!(value(&samples, "uwumail_mail_refused_total", "reason=\"spam\""), 0.0);
    assert_eq!(value(&samples, "uwumail_login_failures_total", "protocol=\"imap\""), 1.0);
    assert_eq!(value(&samples, "uwumail_queue_recipients", "state=\"deferred\""), 0.0);
    assert_eq!(value(&samples, "uwumail_health_level", "area=\"certificate\""), 1.0);
    assert!(value(&samples, "uwumail_certificate_expiry_timestamp_seconds", "") > 1.7e9);
    assert_eq!(value(&samples, "uwumail_alerts_open", "level=\"problem\""), 0.0);

    // Only from the allowed networks: elsewhere even the right token is turned away.
    gate.configure(&MetricsConfig {
        enabled: true,
        token: token.into(),
        allowed_networks: vec!["198.51.100.0/24".into(), "2001:db8::/32".into()],
    })
    .unwrap();
    let (status, _, _) = send(&server.app, scrape(Some(token)), "192.0.2.10").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&server.app, scrape(None), "198.51.100.20").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the network alone is not enough while there is a token");
    let (status, _, _) = send(&server.app, scrape(Some(token)), "2001:db8::7").await;
    assert_eq!(status, StatusCode::OK);

    // Networks without a token: a scraper on a trusted network needs nothing else.
    gate.configure(&MetricsConfig {
        enabled: true,
        token: String::new(),
        allowed_networks: vec!["198.51.100.0/24".into()],
    })
    .unwrap();
    let (status, _, _) = send(&server.app, scrape(None), "198.51.100.20").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&server.app, scrape(Some(token)), "192.0.2.10").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Switched off again: gone.
    gate.configure(&MetricsConfig::default()).unwrap();
    let (status, _, _) = send(&server.app, scrape(None), "198.51.100.20").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guessing_the_metrics_token_is_stopped() {
    let server = server().await;
    let gate = Arc::new(
        MetricsGate::new(&MetricsConfig {
            enabled: true,
            token: "the-right-metrics-token".into(),
            allowed_networks: vec![],
        })
        .unwrap(),
    );
    server.web.set_metrics_gate(gate);
    let mut last = StatusCode::OK;
    for attempt in 0..30 {
        let request = Request::get("/metrics")
            .header(header::AUTHORIZATION, format!("Bearer guess-number-{attempt:04}"))
            .body(Body::empty())
            .unwrap();
        last = send(&server.app, request, "203.0.113.9").await.0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let request =
        Request::get("/metrics").header(header::AUTHORIZATION, "Bearer the-right-metrics-token").body(Body::empty());
    let (status, _, _) = send(&server.app, request.unwrap(), "203.0.113.9").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "not even the right one, for a while");
}
