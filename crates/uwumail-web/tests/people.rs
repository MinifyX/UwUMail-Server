//! Managing people through the portal API: invitations, changes, the trash, aliases and the change log.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

fn smtp(store: Store) -> Smtp {
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    Smtp::new(store, settings).unwrap()
}

struct Portal {
    app: Router,
    store: Store,
    _dir: tempfile::TempDir,
}

/// A logged-in browser: the cookie and the CSRF token.
#[derive(Clone)]
struct Browser {
    cookie: String,
    csrf: String,
}

impl Portal {
    async fn new() -> Portal {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.de").await.unwrap();
        store.create_domain("verein.de").await.unwrap();
        store
            .create_account(NewAccount {
                address: "nyu@example.de".into(),
                display_name: "Nyu".into(),
                password: Some("katzenpfote-123".into()),
                role: Role::Admin,
                quota_bytes: 0,
            })
            .await
            .unwrap();
        let web = Web::new(
            smtp(store.clone()),
            WebSettings { hostname: "mail.example.de".into(), started: Instant::now(), logs: None, config: None },
        );
        Portal { app: web.router(), store, _dir: dir }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        browser: Option<&Browser>,
    ) -> (StatusCode, Option<String>, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(browser) = browser {
            request = request.header(header::COOKIE, &browser.cookie).header(CSRF_HEADER, &browser.csrf);
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let mut request = request.body(body).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, cookie, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn login(&self, login: &str, password: &str) -> Browser {
        let (status, cookie, body) =
            self.request("POST", "/api/auth/login", Some(json!({ "login": login, "password": password })), None).await;
        assert_eq!(status, StatusCode::OK, "login {login}: {body}");
        Browser { cookie: cookie.unwrap(), csrf: body["csrfToken"].as_str().unwrap().to_owned() }
    }
}

#[tokio::test]
async fn invite_a_person_who_then_chooses_a_password() {
    let portal = Portal::new().await;
    let nyu = portal.login("nyu@example.de", "katzenpfote-123").await;

    let (status, _, created) = portal
        .request(
            "POST",
            "/api/admin/people",
            Some(json!({ "address": "Leni@Verein.de", "name": "Leni", "quotaBytes": 1048576 })),
            Some(&nyu),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["person"]["login"], "leni@verein.de");
    assert_eq!(created["person"]["status"], "invited");
    let path = created["link"]["path"].as_str().unwrap();
    let token = path.strip_prefix("/password/").unwrap();

    // The link page shows who it is for; a weak password is refused and does not use up the link.
    let (status, _, link) = portal.request("GET", &format!("/api/password-links/{token}"), None, None).await;
    assert_eq!(
        (status, link["login"].as_str(), link["purpose"].as_str()),
        (StatusCode::OK, Some("leni@verein.de"), Some("invite"))
    );
    let (status, _, body) = portal
        .request("POST", &format!("/api/password-links/{token}"), Some(json!({ "password": "kurz" })), None)
        .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("weakPassword")));

    let (status, cookie, session) = portal
        .request(
            "POST",
            &format!("/api/password-links/{token}"),
            Some(json!({ "password": "Seifenblase-Wanderweg" })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert!(cookie.unwrap().starts_with("__Host-uwumail="), "choosing a password logs you in");
    assert_eq!(session["account"]["login"], "leni@verein.de");

    let (status, _, body) = portal.request("GET", &format!("/api/password-links/{token}"), None, None).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("linkInvalid")));
    let leni = portal.login("leni@verein.de", "Seifenblase-Wanderweg").await;

    // People are for admins only.
    let (status, _, _) = portal.request("GET", "/api/admin/people", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, people) = portal.request("GET", "/api/admin/people", None, Some(&nyu)).await;
    assert_eq!(status, StatusCode::OK);
    let statuses: Vec<_> =
        people.as_array().unwrap().iter().map(|p| (p["login"].clone(), p["status"].clone())).collect();
    assert_eq!(statuses, vec![(json!("leni@verein.de"), json!("active")), (json!("nyu@example.de"), json!("active"))]);

    // A reset link for someone with a password, and the change log has seen it all.
    let (status, _, reset) =
        portal.request("POST", "/api/admin/people/leni@verein.de/password-link", Some(json!({})), Some(&nyu)).await;
    assert_eq!(status, StatusCode::OK);
    let reset_token = reset["path"].as_str().unwrap().strip_prefix("/password/").unwrap().to_owned();
    let (_, _, link) = portal.request("GET", &format!("/api/password-links/{reset_token}"), None, None).await;
    assert_eq!(link["purpose"], "reset");

    let (_, _, log) = portal.request("GET", "/api/admin/audit", None, Some(&nyu)).await;
    let actions: Vec<_> = log.as_array().unwrap().iter().map(|r| r["action"].as_str().unwrap().to_owned()).collect();
    assert_eq!(actions, ["account.passwordLink", "account.passwordChosen", "account.create"]);
    assert!(!log.to_string().contains("Seifenblase"), "the change log never contains passwords");
}

#[tokio::test]
async fn change_lock_out_trash_and_restore() {
    let portal = Portal::new().await;
    let nyu = portal.login("nyu@example.de", "katzenpfote-123").await;
    let new = json!({ "address": "ami@example.de", "name": "Ami", "password": "Kirschbluete-Tastatur" });
    let (status, _, created) = portal.request("POST", "/api/admin/people", Some(new), Some(&nyu)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["link"], Value::Null);

    let (status, _, body) =
        portal.request("PATCH", "/api/admin/people/nyu@example.de", Some(json!({ "admin": false })), Some(&nyu)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("lastAdmin")));
    let (status, _, body) = portal
        .request("PATCH", "/api/admin/people/nyu@example.de", Some(json!({ "disabled": true })), Some(&nyu))
        .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("notYourself")));

    let ami = portal.login("ami@example.de", "Kirschbluete-Tastatur").await;
    let (status, _, person) = portal
        .request(
            "PATCH",
            "/api/admin/people/ami@example.de",
            Some(json!({ "disabled": true, "quotaBytes": 5000 })),
            Some(&nyu),
        )
        .await;
    assert_eq!(
        (status, person["status"].as_str(), person["quotaBytes"].as_i64()),
        (StatusCode::OK, Some("disabled"), Some(5000))
    );
    let (_, _, session) = portal.request("GET", "/api/session", None, Some(&ami)).await;
    assert_eq!(session, Value::Null, "locking someone out ends their sessions");
    assert!(portal.store.resolve_recipient("ami@example.de").await.unwrap().is_some(), "mail still arrives");

    portal.request("PATCH", "/api/admin/people/ami@example.de", Some(json!({ "disabled": false })), Some(&nyu)).await;
    let (status, _, alias) = portal
        .request(
            "POST",
            "/api/admin/people/ami@example.de/aliases",
            Some(json!({ "address": "info@verein.de" })),
            Some(&nyu),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(alias["addresses"][1]["address"], "info@verein.de");
    let (status, _, _) = portal
        .request(
            "POST",
            "/api/admin/people/nyu@example.de/aliases",
            Some(json!({ "address": "info@verein.de" })),
            Some(&nyu),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "an address belongs to one person");

    let (status, _, trashed) = portal.request("DELETE", "/api/admin/people/ami@example.de", None, Some(&nyu)).await;
    assert_eq!((status, trashed["status"].as_str()), (StatusCode::OK, Some("deleted")));
    assert!(trashed["purgeAt"].as_i64().unwrap() > trashed["deletedAt"].as_i64().unwrap());
    assert!(portal.store.resolve_recipient("info@verein.de").await.unwrap().is_none());
    let (status, _, _) = portal.request("DELETE", "/api/admin/people/nyu@example.de", None, Some(&nyu)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _, restored) =
        portal.request("POST", "/api/admin/people/ami@example.de/restore", Some(json!({})), Some(&nyu)).await;
    assert_eq!((status, restored["status"].as_str()), (StatusCode::OK, Some("active")));
    let (status, _, _) =
        portal.request("DELETE", "/api/admin/people/ami@example.de/aliases/info@verein.de", None, Some(&nyu)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = portal
        .request(
            "POST",
            "/api/admin/people/ami@example.de/purge",
            Some(json!({ "confirm": "ami@verein.de" })),
            Some(&nyu),
        )
        .await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("confirmationMismatch")));
    let (status, _, _) = portal
        .request(
            "POST",
            "/api/admin/people/ami@example.de/purge",
            Some(json!({ "confirm": "AMI@example.de" })),
            Some(&nyu),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(portal.store.account("ami@example.de").await.unwrap().is_none());

    let (_, _, log) = portal.request("GET", "/api/admin/audit?limit=3", None, Some(&nyu)).await;
    let actions: Vec<_> = log.as_array().unwrap().iter().map(|r| r["action"].as_str().unwrap().to_owned()).collect();
    assert_eq!(actions, ["account.purge", "alias.remove", "account.restore"]);
}
