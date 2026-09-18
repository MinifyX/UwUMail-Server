//! Domains through the portal API: add, catch-all, DKIM rotation, remove.

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

async fn portal() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
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
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let smtp = Smtp::new(store.clone(), settings).unwrap();
    let web = Web::new(
        smtp,
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            config: None,
            certificate: None,
        },
    );
    (web.router(), store, dir)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&(String, String)>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = auth {
        request = request.header(header::COOKIE, cookie).header(CSRF_HEADER, csrf);
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
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.split(';').next().unwrap().into());
    }
    (status, json)
}

fn states(domain: &Value) -> Vec<(String, String)> {
    let mut states: Vec<_> = domain["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| (key["selector"].as_str().unwrap().to_owned(), key["state"].as_str().unwrap().to_owned()))
        .collect();
    states.sort();
    states
}

#[tokio::test]
async fn domains_with_catch_all_and_key_rotation() {
    let (app, store, _dir) = portal().await;
    let (_, login) = call(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({ "login": "nyu@example.de", "password": "katzenpfote-123" })),
        None,
    )
    .await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let (status, created) =
        call(&app, "POST", "/api/admin/domains", Some(json!({ "name": "Verein.DE" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "verein.de");
    let first = states(&created);
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|(_, state)| state == "active"));
    assert!(created["keys"][0]["dnsName"].as_str().unwrap().ends_with("._domainkey.verein.de"));
    assert_eq!(created["setup"]["hostname"], "mail.example.de");

    let (status, _) = call(&app, "POST", "/api/admin/domains", Some(json!({ "name": "verein.de" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (_, list) = call(&app, "GET", "/api/admin/domains", None, Some(&auth)).await;
    let names: Vec<_> = list.as_array().unwrap().iter().map(|d| (d["name"].clone(), d["people"].clone())).collect();
    assert_eq!(names, vec![(json!("example.de"), json!(1)), (json!("verein.de"), json!(0))]);

    // Catch-all to a person, then off again.
    let (status, domain) = call(
        &app,
        "PUT",
        "/api/admin/domains/verein.de/catch-all",
        Some(json!({ "login": "nyu@example.de" })),
        Some(&auth),
    )
    .await;
    assert_eq!((status, domain["catchAll"].as_str()), (StatusCode::OK, Some("nyu@example.de")));
    assert!(store.resolve_recipient("irgendwer@verein.de").await.unwrap().is_some());
    let (_, domain) =
        call(&app, "PUT", "/api/admin/domains/verein.de/catch-all", Some(json!({ "login": null })), Some(&auth)).await;
    assert_eq!(domain["catchAll"], Value::Null);

    // A forwarding address keeps the domain in use until it is gone.
    let forward = json!({ "local": "Kasse", "targets": ["kassenwart@example.org"], "note": "Beiträge" });
    let (status, domain) = call(&app, "PUT", "/api/admin/domains/verein.de/forwards", Some(forward), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{domain}");
    assert_eq!(domain["forwards"][0]["address"], "kasse@verein.de");
    assert_eq!(domain["forwards"][0]["targets"], json!(["kassenwart@example.org"]));
    let (status, refused) = call(&app, "DELETE", "/api/admin/domains/verein.de", None, Some(&auth)).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("domainInUse")));
    let (status, domain) = call(&app, "DELETE", "/api/admin/domains/verein.de/forwards/kasse", None, Some(&auth)).await;
    assert_eq!((status, domain["forwards"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)));

    // Rotation: new keys wait, switch over (forced, no DNS in tests), the old ones retire and can go.
    let (_, rotating) =
        call(&app, "POST", "/api/admin/domains/verein.de/dkim/rotate", Some(json!({})), Some(&auth)).await;
    let pending: Vec<_> = states(&rotating).into_iter().filter(|(_, state)| state == "pending").collect();
    assert_eq!(pending.len(), 2, "{rotating}");
    let (_, again) = call(&app, "POST", "/api/admin/domains/verein.de/dkim/rotate", Some(json!({})), Some(&auth)).await;
    assert_eq!(states(&again).len(), 4, "preparing twice keeps the same new keys");

    let (status, switched) =
        call(&app, "POST", "/api/admin/domains/verein.de/dkim/activate", Some(json!({ "force": true })), Some(&auth))
            .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    let after = states(&switched);
    assert_eq!(after.iter().filter(|(_, state)| state == "active").count(), 2);
    let retired: Vec<_> =
        after.iter().filter(|(_, state)| state == "retired").map(|(selector, _)| selector.clone()).collect();
    assert_eq!(retired.len(), 2);
    let active = after.iter().find(|(_, state)| state == "active").unwrap().0.clone();
    let (status, body) =
        call(&app, "DELETE", &format!("/api/admin/domains/verein.de/dkim/{active}"), None, Some(&auth)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("keyActive")));
    let (status, trimmed) =
        call(&app, "DELETE", &format!("/api/admin/domains/verein.de/dkim/{}", retired[0]), None, Some(&auth)).await;
    assert_eq!((status, states(&trimmed).len()), (StatusCode::OK, 3));

    // Domains with addresses stay.
    let (status, body) = call(&app, "DELETE", "/api/admin/domains/example.de", None, Some(&auth)).await;
    assert_eq!((status, body["code"].as_str()), (StatusCode::CONFLICT, Some("domainInUse")));
    let (status, _) = call(&app, "DELETE", "/api/admin/domains/verein.de", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, log) = call(&app, "GET", "/api/admin/audit?limit=20", None, Some(&auth)).await;
    let actions: Vec<_> = log.as_array().unwrap().iter().map(|r| r["action"].as_str().unwrap().to_owned()).collect();
    assert_eq!(
        actions,
        [
            "domain.remove",
            "domain.dkimRemove",
            "domain.dkimActivate",
            "domain.dkimPrepare",
            "domain.dkimPrepare",
            "domain.forwardAddressRemove",
            "domain.forwardAddress",
            "domain.catchAll",
            "domain.catchAll",
            "domain.create"
        ]
    );
}

#[tokio::test]
async fn mta_sts_policy_and_reports() {
    let (app, store, _dir) = portal().await;
    let (_, login) = call(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({ "login": "nyu@example.de", "password": "katzenpfote-123" })),
        None,
    )
    .await;
    let auth = (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned());

    let fetch_policy = |host: &'static str| {
        let app = app.clone();
        async move {
            let request =
                Request::get("/.well-known/mta-sts.txt").header(header::HOST, host).body(Body::empty()).unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
            (status, String::from_utf8(bytes.to_vec()).unwrap())
        }
    };
    assert_eq!(fetch_policy("mta-sts.example.de").await.0, StatusCode::NOT_FOUND, "off by default");

    let path = "/api/admin/domains/example.de/mta-sts";
    let (status, detail) = call(&app, "PUT", path, Some(json!({ "mode": "testing" })), Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        (detail["mtaSts"]["mode"].clone(), detail["mtaSts"]["mx"].clone()),
        (json!("testing"), json!(["mail.example.de"]))
    );
    let (status, policy) = fetch_policy("MTA-STS.example.de:443").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy, "version: STSv1\r\nmode: testing\r\nmx: mail.example.de\r\nmax_age: 86400\r\n");
    assert_eq!(policy, detail["mtaSts"]["policy"]);
    assert_eq!(fetch_policy("mta-sts.elsewhere.example").await.0, StatusCode::NOT_FOUND);

    // Senders would insist on a valid certificate, and this server has none.
    let (_, refused) = call(&app, "PUT", path, Some(json!({ "mode": "enforce" })), Some(&auth)).await;
    assert_eq!(refused["code"], "mtaStsCertificate");
    let (_, off) = call(&app, "PUT", path, Some(json!({ "mode": "off" })), Some(&auth)).await;
    assert_eq!(off["mtaSts"], Value::Null);

    store
        .add_tls_report(uwumail_store::NewTlsReport {
            domain: "example.de".into(),
            organization: "reporter.example".into(),
            report_id: "t1".into(),
            begin_at: 0,
            end_at: i64::MAX / 2,
            authenticated: true,
            successful: 5,
            failed: 1,
            failures: vec![],
            ..Default::default()
        })
        .await
        .unwrap();
    let (status, reports) = call(&app, "GET", "/api/admin/domains/example.de/reports?days=7", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{reports}");
    assert_eq!((reports["tls"]["successful"].clone(), reports["tls"]["failed"].clone()), (json!(5), json!(1)));
    assert_eq!((reports["days"].clone(), reports["suggestions"].clone()), (json!(7), json!([])));

    // The section that shows every domain at once.
    let (status, overview) = call(&app, "GET", "/api/admin/reports?days=7", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    let first = &overview["domains"][0];
    assert_eq!(first["name"], json!("example.de"));
    assert_eq!(first["tls"]["successful"], json!(5));
    assert_eq!(first["reading"], json!({ "dmarc": true, "tls": true }));

    // And one report on its own, listed and then read.
    let (status, listed) = call(&app, "GET", "/api/admin/domains/example.de/reports/tls", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let entry = &listed["reports"][0];
    assert_eq!(
        (entry["organization"].clone(), entry["good"].clone(), entry["bad"].clone()),
        (json!("reporter.example"), json!(5), json!(1))
    );
    let id = entry["id"].as_i64().unwrap();
    let (status, detail) =
        call(&app, "GET", &format!("/api/admin/domains/example.de/reports/tls/{id}"), None, Some(&auth)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!((detail["kind"].clone(), detail["report"]["reportId"].clone()), (json!("tls"), json!("t1")));

    let (status, gone) = call(&app, "GET", "/api/admin/domains/example.de/reports/tls/999999", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{gone}");
    let (status, nonsense) =
        call(&app, "GET", "/api/admin/domains/example.de/reports/nonsense", None, Some(&auth)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{nonsense}");

    let (_, health) = call(&app, "GET", "/api/admin/health", None, Some(&auth)).await;
    let dns = health["areas"].as_array().unwrap().iter().find(|area| area["area"] == "dns").unwrap();
    assert!(dns["findings"].as_array().unwrap().iter().any(|f| f["code"] == "tlsFailures"), "{dns}");
}
