//! Logging in with the password of an LDAP directory (docs/login-oidc-ldap.md), against a minimal
//! directory in this process that speaks just enough LDAP (BER): simple binds and searches.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{AppScope, MailAuth, MailAuthDenied, NewAccount, Role, Store};
use uwumail_web::external::ldap;
use uwumail_web::{AuthConfig, CSRF_HEADER, LdapConfig, Web, WebSettings};

const SERVICE_DN: &str = "cn=reader,dc=example,dc=org";
const SERVICE_PASSWORD: &str = "lesen-darf-ich";
const LENI_PASSWORD: &str = "aus-dem-verzeichnis";
const NYU_PASSWORD: &str = "nyus-verzeichnis-passwort";
const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=org";

// ---- A directory ----

struct Entry {
    dn: &'static str,
    password: &'static str,
    attributes: Vec<(&'static str, Vec<&'static str>)>,
}

#[derive(Default)]
struct Seen {
    /// Every bind: the name and whether a password came with it.
    binds: Vec<(String, bool)>,
    /// Every search filter, written back as text.
    filters: Vec<String>,
}

struct Directory {
    entries: Vec<Entry>,
    seen: Mutex<Seen>,
}

fn directory() -> Directory {
    let person = |dn, password, uid, mail: Vec<&'static str>, cn, groups: Vec<&'static str>| Entry {
        dn,
        password,
        attributes: vec![
            ("objectClass", vec!["top", "person", "inetOrgPerson"]),
            ("uid", vec![uid]),
            ("mail", mail),
            ("cn", vec![cn]),
            ("memberOf", groups),
        ],
    };
    Directory {
        entries: vec![
            Entry {
                dn: SERVICE_DN,
                password: SERVICE_PASSWORD,
                attributes: vec![("objectClass", vec!["applicationProcess"])],
            },
            person(
                "uid=leni,ou=people,dc=example,dc=org",
                LENI_PASSWORD,
                "leni",
                vec!["leni@example.org"],
                "Leni Lindwurm",
                vec![ADMINS],
            ),
            person(
                "uid=nyu,ou=people,dc=example,dc=org",
                NYU_PASSWORD,
                "nyu",
                vec!["nyu@example.org", "nyu.alt@example.org"],
                "Nyu",
                vec![],
            ),
        ],
        seen: Mutex::default(),
    }
}

/// One BER element: its tag, its contents, and what follows it.
fn element(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = data.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let length = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        let (bytes, after) = rest.split_at_checked(count)?;
        rest = after;
        bytes.iter().fold(0usize, |length, byte| length << 8 | usize::from(*byte))
    };
    let (contents, rest) = rest.split_at_checked(length)?;
    Some((tag, contents, rest))
}

fn elements(mut data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut all = Vec::new();
    while let Some((tag, contents, rest)) = element(data) {
        all.push((tag, contents));
        data = rest;
    }
    all
}

fn encode(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match contents.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n < 0x100 => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(contents);
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A filter (RFC 4511 section 4.5.1) as RFC 4515 text, escaping what needs it, and whether it
/// matches an entry.
fn filter(tag: u8, contents: &[u8], entry: Option<&Entry>) -> (String, bool) {
    let values = |name: &str| -> Vec<String> {
        entry
            .into_iter()
            .flat_map(|entry| entry.attributes.iter())
            .filter(|(attribute, _)| attribute.eq_ignore_ascii_case(name))
            .flat_map(|(_, values)| values.iter().map(|value| value.to_lowercase()))
            .collect()
    };
    let escape = |value: &str| {
        value.chars().fold(String::new(), |mut out, c| {
            match c {
                '*' | '(' | ')' | '\\' | '\0' => out.push_str(&format!("\\{:02x}", c as u32)),
                c => out.push(c),
            }
            out
        })
    };
    match tag {
        0xa0 | 0xa1 => {
            let parts: Vec<(String, bool)> = elements(contents).into_iter().map(|(t, c)| filter(t, c, entry)).collect();
            let joined: String = parts.iter().map(|(text, _)| text.as_str()).collect();
            let matched = if tag == 0xa0 { parts.iter().all(|(_, m)| *m) } else { parts.iter().any(|(_, m)| *m) };
            (format!("({}{joined})", if tag == 0xa0 { '&' } else { '|' }), matched)
        }
        0xa2 => {
            let (inner_tag, inner, _) = element(contents).unwrap();
            let (inner_text, matched) = filter(inner_tag, inner, entry);
            (format!("(!{inner_text})"), !matched)
        }
        0xa3 => {
            let parts = elements(contents);
            let (name, value) = (text(parts[0].1), text(parts[1].1));
            let matched = values(&name).contains(&value.to_lowercase());
            (format!("({name}={})", escape(&value)), matched)
        }
        0xa4 => {
            let parts = elements(contents);
            let name = text(parts[0].1);
            let pieces: Vec<(u8, String)> =
                elements(parts[1].1).into_iter().map(|(t, c)| (t, text(c).to_lowercase())).collect();
            let matched = values(&name).iter().any(|value| {
                let mut rest = value.as_str();
                pieces.iter().all(|(kind, piece)| match kind {
                    0x80 => rest.strip_prefix(piece.as_str()).map(|after| rest = after).is_some(),
                    0x82 => rest.ends_with(piece.as_str()),
                    _ => rest.find(piece.as_str()).map(|at| rest = &rest[at + piece.len()..]).is_some(),
                })
            });
            let shown: Vec<String> = pieces.iter().map(|(_, piece)| escape(piece)).collect();
            (format!("({name}=*{}*)", shown.join("*")), matched)
        }
        0x87 => {
            let name = text(contents);
            (format!("({name}=*)"), entry.is_some() && !values(&name).is_empty())
        }
        other => panic!("filter type {other:#x} is not spoken here"),
    }
}

fn result(message_id: &[u8], tag: u8, code: u8) -> Vec<u8> {
    let mut body = encode(0x0a, &[code]);
    body.extend(encode(0x04, b""));
    body.extend(encode(0x04, b""));
    let mut message = encode(0x02, message_id);
    message.extend(encode(tag, &body));
    encode(0x30, &message)
}

fn search_entry(message_id: &[u8], entry: &Entry, wanted: &[String]) -> Vec<u8> {
    let mut attributes = Vec::new();
    for (name, values) in &entry.attributes {
        let all = wanted.is_empty() || wanted.iter().any(|w| w == "*");
        if !all && !wanted.iter().any(|w| w.eq_ignore_ascii_case(name)) {
            continue;
        }
        let set: Vec<u8> = values.iter().flat_map(|value| encode(0x04, value.as_bytes())).collect();
        let mut attribute = encode(0x04, name.as_bytes());
        attribute.extend(encode(0x31, &set));
        attributes.extend(encode(0x30, &attribute));
    }
    let mut body = encode(0x04, entry.dn.as_bytes());
    body.extend(encode(0x30, &attributes));
    let mut message = encode(0x02, message_id);
    message.extend(encode(0x64, &body));
    encode(0x30, &message)
}

async fn serve_connection(directory: Arc<Directory>, mut stream: TcpStream) {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let Ok(read) = stream.read(&mut chunk).await else { return };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        while let Some((0x30, message, rest)) = element(&buffer) {
            let consumed = buffer.len() - rest.len();
            let parts = elements(message);
            let (message_id, (op, body)) = (parts[0].1, parts[1]);
            let mut answer = Vec::new();
            match op {
                // BindRequest: version, name, simple password.
                0x60 => {
                    let fields = elements(body);
                    let (name, password) = (text(fields[1].1), text(fields[2].1));
                    directory.seen.lock().unwrap().binds.push((name.clone(), !password.is_empty()));
                    // An empty password is an unauthenticated bind, which real directories allow.
                    let code = if password.is_empty() {
                        0
                    } else {
                        let right = directory
                            .entries
                            .iter()
                            .any(|e| e.dn.eq_ignore_ascii_case(&name) && e.password == password);
                        if right { 0 } else { 49 }
                    };
                    answer.extend(result(message_id, 0x61, code));
                }
                // SearchRequest: base, scope, deref, limits, typesOnly, filter, attributes.
                0x63 => {
                    let fields = elements(body);
                    let base = text(fields[0].1).to_lowercase();
                    let scope = fields[1].1[0];
                    let (filter_tag, filter_body) = fields[6];
                    let wanted: Vec<String> = elements(fields[7].1).into_iter().map(|(_, c)| text(c)).collect();
                    directory.seen.lock().unwrap().filters.push(filter(filter_tag, filter_body, None).0);
                    for entry in &directory.entries {
                        let dn = entry.dn.to_lowercase();
                        let inside =
                            if scope == 0 { dn == base } else { dn == base || dn.ends_with(&format!(",{base}")) };
                        if inside && filter(filter_tag, filter_body, Some(entry)).1 {
                            answer.extend(search_entry(message_id, entry, &wanted));
                        }
                    }
                    answer.extend(result(message_id, 0x65, 0));
                }
                // UnbindRequest.
                0x42 => return,
                other => panic!("LDAP operation {other:#x} is not spoken here"),
            }
            if stream.write_all(&answer).await.is_err() {
                return;
            }
            buffer.drain(..consumed);
        }
    }
}

async fn start_directory() -> (Arc<Directory>, SocketAddr) {
    let directory = Arc::new(directory());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = directory.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_connection(serving.clone(), stream));
        }
    });
    (directory, address)
}

fn config(address: SocketAddr) -> LdapConfig {
    LdapConfig {
        enabled: true,
        url: format!("ldap://{address}"),
        // Only a directory on this very machine may go without TLS.
        starttls: false,
        insecure_localhost: true,
        bind_dn: SERVICE_DN.into(),
        bind_password: SERVICE_PASSWORD.into(),
        base_dn: "ou=people,dc=example,dc=org".into(),
        admin_group_dn: ADMINS.into(),
        auto_create: true,
        allowed_domains: vec!["example.org".into()],
        ..LdapConfig::default()
    }
}

// ---- The portal ----

fn web(store: &Store, ldap: LdapConfig) -> Web {
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    );
    web.external_login().configure(AuthConfig { ldap, ..AuthConfig::default() });
    web
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    auth: Option<&(String, String)>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
    }
    let mut request = request.body(Body::from(body.to_string())).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|value| value.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), cookie)
}

async fn login(app: &Router, login: &str, password: &str) -> Option<(String, String)> {
    let (status, body, cookie) =
        call(app, "POST", "/api/auth/login", json!({ "login": login, "password": password }), None).await;
    (status == StatusCode::OK).then(|| (cookie.unwrap(), body["csrfToken"].as_str().unwrap().to_owned()))
}

#[tokio::test]
async fn directory_passwords_for_the_portal_and_mail_apps() {
    let (directory, address) = start_directory().await;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    // Nyu has an account of her own already, with a password here.
    store
        .create_account(NewAccount {
            address: "nyu@example.org".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let web = web(&store, config(address));
    let app = web.router();

    // Someone the directory knows gets an account at the first login, an admin by their group.
    assert!(login(&app, "leni@example.org", "falsch-falsch").await.is_none());
    assert!(store.account("leni@example.org").await.unwrap().is_none());
    let leni = login(&app, "Leni@example.org", LENI_PASSWORD).await.expect("the directory's password works");
    let account = store.account("leni@example.org").await.unwrap().unwrap();
    assert_eq!((account.display_name.as_str(), account.role), ("Leni Lindwurm", Role::Admin));
    assert_eq!(store.auth_source(account.id).await.unwrap(), "ldap");
    assert!(
        directory.seen.lock().unwrap().binds.iter().any(|(name, _)| name == SERVICE_DN),
        "found by the service account"
    );
    assert!(
        directory.seen.lock().unwrap().filters.contains(&"(&(objectClass=person)(mail=leni@example.org))".to_owned()),
        "{:?}",
        directory.seen.lock().unwrap().filters
    );

    // Mail apps may use it too, while main passwords are allowed; wrong ones stay wrong.
    let mail = store.authenticate_mail("leni@example.org", LENI_PASSWORD, AppScope::Mail, "imap", "192.0.2.1").await;
    assert!(matches!(mail.unwrap(), MailAuth::Ok { app_password: None, .. }));
    let wrong = store.authenticate_mail("leni@example.org", "falsch", AppScope::Mail, "imap", "192.0.2.1").await;
    assert!(matches!(wrong.unwrap(), MailAuth::Denied(MailAuthDenied::Invalid)));

    // An empty password never reaches the directory, where it would be an unauthenticated bind.
    let binds = directory.seen.lock().unwrap().binds.len();
    assert!(login(&app, "leni@example.org", "").await.is_none());
    let empty = store.authenticate_mail("leni@example.org", "", AppScope::Mail, "imap", "192.0.2.1").await;
    assert!(matches!(empty.unwrap(), MailAuth::Denied(_)));
    assert_eq!(ldap::authenticate(&config(address), "leni@example.org", "").await, Ok(None));
    assert_eq!(directory.seen.lock().unwrap().binds.len(), binds, "no bind at all");

    // An account that checks its password here does not take the directory's.
    assert!(login(&app, "nyu@example.org", NYU_PASSWORD).await.is_none());
    // The admin moves Nyu to the directory: her old password stops working, the directory's works.
    let (status, _, _) =
        call(&app, "PUT", "/api/admin/people/nyu@example.org/auth-source", json!({ "source": "ldap" }), Some(&leni))
            .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(login(&app, "nyu@example.org", "katzenpfote-123").await.is_none());
    assert!(login(&app, "nyu@example.org", NYU_PASSWORD).await.is_some());
    let (_, person, _) = call(&app, "GET", "/api/admin/people/nyu@example.org", Value::Null, Some(&leni)).await;
    assert_eq!(person["authSource"], "ldap");
    assert_eq!(person["status"], "active", "not waiting for an invitation: {person}");
    let set = json!({ "password": "ein-ganz-neues-passwort-17" });
    let (status, refused, _) = call(&app, "PUT", "/api/admin/people/nyu@example.org/password", set, Some(&leni)).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("passwordInDirectory")), "{refused}");
    // Her password is changed at the directory, not here.
    let nyu = login(&app, "nyu@example.org", NYU_PASSWORD).await.unwrap();
    let change = json!({ "current": NYU_PASSWORD, "new": "ein-ganz-neues-passwort-17" });
    let (status, refused, _) = call(&app, "POST", "/api/account/password", change, Some(&nyu)).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("passwordInDirectory")), "{refused}");

    // Addresses outside the allowed domains get no account.
    assert!(login(&app, "leni@example.net", LENI_PASSWORD).await.is_none());

    // The admin tries the settings.
    let (status, tried, _) =
        call(&app, "POST", "/api/admin/auth/ldap/test", json!({ "changes": {} }), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{tried}");
    assert!(tried["detail"].as_str().unwrap().contains("ou=people,dc=example,dc=org"));
    let (status, _, _) = call(&app, "POST", "/api/admin/auth/ldap/test", json!({ "changes": {} }), Some(&nyu)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "admins only");
    let broken = LdapConfig { bind_password: "falsch".into(), ..config(address) };
    let refused = ldap::test(&broken).await.unwrap_err();
    assert!(refused.contains("service account"), "{refused}");

    // Nyu's mailbox becomes a service: the directory's password opens nothing anymore.
    let (status, _, _) =
        call(&app, "PATCH", "/api/admin/people/nyu@example.org", json!({ "service": true }), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(&app, "GET", "/api/account/addresses", Value::Null, Some(&nyu)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "her session went");
    assert!(login(&app, "nyu@example.org", NYU_PASSWORD).await.is_none());
    for protocol in ["imap", "smtp", "jmap"] {
        let mail =
            store.authenticate_mail("nyu@example.org", NYU_PASSWORD, AppScope::Mail, protocol, "192.0.2.1").await;
        assert!(matches!(mail.unwrap(), MailAuth::Denied(MailAuthDenied::Invalid)), "{protocol}");
    }
    let (_, person, _) = call(&app, "GET", "/api/admin/people/nyu@example.org", Value::Null, Some(&leni)).await;
    assert_eq!(person["authSource"], "local");
}

#[tokio::test]
async fn what_is_typed_stays_a_value_in_filters_and_names() {
    let (directory, address) = start_directory().await;

    // A filter that only looks at the part before the @: an injected wildcard stays a character.
    let by_uid = LdapConfig { user_filter: "(&(objectClass=person)(uid={user}))".into(), ..config(address) };
    assert_eq!(ldap::authenticate(&by_uid, "le*@example.org", LENI_PASSWORD).await, Ok(None));
    assert_eq!(ldap::authenticate(&by_uid, "*)(uid=leni@example.org", LENI_PASSWORD).await, Ok(None));
    let filters = directory.seen.lock().unwrap().filters.clone();
    assert!(filters.contains(&"(&(objectClass=person)(uid=le\\2a))".to_owned()), "{filters:?}");
    assert!(filters.contains(&"(&(objectClass=person)(uid=\\2a\\29\\28uid=leni))".to_owned()), "{filters:?}");
    let found = ldap::authenticate(&by_uid, "leni@example.org", LENI_PASSWORD).await.unwrap().expect("leni");
    assert_eq!(found.dn, "uid=leni,ou=people,dc=example,dc=org");
    assert!(found.admin && found.has_address("leni@example.org"));
    // `leni@` of another domain is not the directory's Leni, whose address it knows.
    let other = ldap::authenticate(&by_uid, "leni@example.net", LENI_PASSWORD).await.unwrap().unwrap();
    assert!(!other.has_address("leni@example.net"));
    let nyu = ldap::authenticate(&by_uid, "nyu@example.org", NYU_PASSWORD).await.unwrap().unwrap();
    assert!(nyu.has_address("nyu.alt@example.org"), "every value of the mail attribute counts");

    // A DN made from a template: a comma typed into the login cannot add parts to the name.
    let template = LdapConfig {
        user_dn_template: "uid={user},ou=people,dc=example,dc=org".into(),
        bind_dn: String::new(),
        bind_password: String::new(),
        ..config(address)
    };
    assert!(ldap::authenticate(&template, "leni@example.org", LENI_PASSWORD).await.unwrap().is_some());
    assert_eq!(ldap::authenticate(&template, "nyu,ou=people@example.org", NYU_PASSWORD).await, Ok(None));
    let binds = directory.seen.lock().unwrap().binds.clone();
    assert!(binds.iter().any(|(name, _)| name == "uid=nyu\\2cou\\3dpeople,ou=people,dc=example,dc=org"), "{binds:?}");
    assert!(binds.iter().all(|(_, password)| *password), "never a bind without a password: {binds:?}");
}
