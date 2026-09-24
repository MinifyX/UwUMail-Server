//! The spam filter in My account and for admins: what the Bayes filter learned, and learning from
//! mail that is already sorted.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

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
    let cookie =
        response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if let (Some(cookie), Value::Object(map)) = (cookie, &mut json) {
        map.insert("_cookie".into(), cookie.into());
    }
    (status, json)
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let body = json!({ "login": address, "password": "katzenpfote-123" });
    let (_, login) = call(app, "POST", "/api/auth/login", Some(body), None).await;
    (login["_cookie"].as_str().unwrap().to_owned(), login["csrfToken"].as_str().unwrap().to_owned())
}

/// A server with the domain example.de, the admin chef and Leni; returns their account ids.
async fn server() -> (tempfile::TempDir, Store, Vec<i64>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    let mut ids = Vec::new();
    for (address, role) in [("chef@example.de", Role::Admin), ("leni@example.de", Role::User)] {
        let account = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: Some("katzenpfote-123".into()),
            role,
            quota_bytes: 0,
            protocols: None,
        };
        ids.push(store.create_account(account).await.unwrap().id);
    }
    (dir, store, ids)
}

fn router(store: &Store) -> Router {
    let settings = SmtpSettings {
        hostname: "mail.example.de".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.de".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    web.router()
}

#[tokio::test]
async fn people_and_admins_see_what_was_learned_and_can_learn_from_sorted_mail() {
    let (_dir, store, ids) = server().await;
    let month_ago = now_secs() - 30 * 24 * 3600;
    for (raw, role, keywords, received_at) in [
        (&b"Subject: Gewinnspiel\r\n\r\ngratis\r\n"[..], MailboxRole::Junk, vec![], None),
        (
            &b"Subject: Elternabend\r\n\r\nDienstag\r\n"[..],
            MailboxRole::Inbox,
            vec!["$seen".to_owned()],
            Some(month_ago),
        ),
    ] {
        let request = IngestRequest {
            account_id: ids[1],
            raw: raw.to_vec(),
            mailboxes: vec![MailboxTarget::Role(role)],
            keywords,
            received_at,
        };
        store.ingest(request).await.unwrap();
    }
    let app = router(&store);

    let leni = login(&app, "leni@example.de").await;
    let (status, overview) = call(&app, "GET", "/api/account/spam", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["bayes"]["own"], json!({ "spam": 0, "ham": 0 }));
    assert_eq!((overview["bayes"]["minimum"].as_i64(), overview["bayes"]["enabled"].as_bool()), (Some(50), Some(true)));

    let (status, learned) = call(&app, "POST", "/api/account/spam/learn-folders", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{learned}");
    assert_eq!(learned, json!({ "spam": 1, "ham": 1 }));
    let (status, _) = call(&app, "GET", "/api/admin/spam", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the server's numbers are for admins");

    let chef = login(&app, "chef@example.de").await;
    let (status, overview) = call(&app, "GET", "/api/admin/spam", None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{overview}");
    assert_eq!(overview["bayes"]["queued"].as_i64(), Some(4), "each message for the server and for Leni");
    let (status, learned) = call(&app, "POST", "/api/admin/spam/learn-folders", None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{learned}");
    assert_eq!(learned, json!({ "spam": 1, "ham": 1, "people": 1 }));
    let audit = store.audit_log(10, None).await.unwrap();
    assert!(audit.iter().any(|entry| entry.action == "spam.learnFromFolders"));
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[tokio::test]
async fn people_set_their_own_spam_limits() {
    let (_dir, store, _ids) = server().await;
    let app = router(&store);
    let leni = login(&app, "leni@example.de").await;

    let (status, overview) = call(&app, "GET", "/api/account/spam", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["limits"]["own"], json!({ "junk": null, "reject": null }));
    assert_eq!(overview["limits"]["server"]["junk"], 5.0);

    let limits = json!({ "junk": 8.0, "reject": 20.0 });
    let (status, saved) = call(&app, "PUT", "/api/account/spam/limits", Some(limits.clone()), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["own"], limits);
    let reversed = json!({ "junk": 20.0, "reject": 8.0 });
    let (status, refused) = call(&app, "PUT", "/api/account/spam/limits", Some(reversed), Some(&leni)).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("spamLimitsOrder")), "{refused}");
    let (status, _) = call(&app, "PUT", "/api/account/spam/limits", Some(json!({ "junk": 0.5 })), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn people_keep_their_own_sender_lists_and_admins_those_of_the_server_and_domains() {
    let (_dir, store, _ids) = server().await;
    let app = router(&store);
    let leni = login(&app, "leni@example.de").await;
    let chef = login(&app, "chef@example.de").await;

    let body = json!({ "list": "block", "value": "Werbung@Example.com", "note": "Newsletter" });
    let (status, view) = call(&app, "POST", "/api/account/spam/senders", Some(body), Some(&leni)).await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    let entry = &view["entries"][0];
    assert_eq!((entry["kind"].as_str(), entry["value"].as_str()), (Some("address"), Some("werbung@example.com")));
    assert_eq!(view["limit"].as_i64(), Some(1000));
    let id = entry["id"].as_i64().unwrap();

    let body = json!({ "list": "allow", "value": "werbung@example.com" });
    let (status, error) = call(&app, "POST", "/api/account/spam/senders", Some(body), Some(&leni)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("senderListed")));
    let body = json!({ "list": "block", "kind": "ip", "value": "example.com" });
    let (status, error) = call(&app, "POST", "/api/account/spam/senders", Some(body), Some(&leni)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("senderInvalid")));

    let (status, _) = call(&app, "DELETE", &format!("/api/account/spam/senders/{id}"), None, Some(&chef)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nobody removes someone else's entry, admins neither");
    let (status, _) = call(&app, "GET", "/api/admin/spam/senders", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let body = json!({ "list": "allow", "value": "192.0.2.10", "domain": "example.de" });
    let (status, view) = call(&app, "POST", "/api/admin/spam/senders", Some(body), Some(&chef)).await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    assert_eq!(view["entries"][0]["domain"].as_str(), Some("example.de"));
    assert_eq!(view["domains"], json!(["example.de"]));
    let body = json!({ "list": "block", "value": "*.spam.example" });
    let (status, view) = call(&app, "POST", "/api/admin/spam/senders", Some(body), Some(&chef)).await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    assert_eq!(view["entries"].as_array().map(Vec::len), Some(2), "Leni's own entry is not the admins' business");
    let body = json!({ "list": "block", "value": "spam.example", "domain": "unknown.example" });
    let (status, _) = call(&app, "POST", "/api/admin/spam/senders", Some(body), Some(&chef)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let host = view["entries"].as_array().unwrap().iter().find(|entry| entry["kind"] == "host").unwrap();
    let path = format!("/api/admin/spam/senders/{}", host["id"]);
    let (status, view) = call(&app, "DELETE", &path, None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let actions: Vec<String> = store.audit_log(10, None).await.unwrap().into_iter().map(|entry| entry.action).collect();
    assert!(actions.contains(&"spam.senderAdd".to_owned()) && actions.contains(&"spam.senderRemove".to_owned()));

    let (status, view) = call(&app, "DELETE", &format!("/api/account/spam/senders/{id}"), None, Some(&leni)).await;
    assert_eq!((status, view["entries"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)));
}

#[tokio::test]
async fn people_and_admins_keep_word_lists_and_see_the_built_in_lists() {
    let (_dir, store, _ids) = server().await;
    let app = router(&store);
    let leni = login(&app, "leni@example.de").await;
    let chef = login(&app, "chef@example.de").await;

    let body = json!({ "text": "# meine Liste\nCasino\n/\\sjackpot\\s/i\ncasino\n/(?=x)/\n", "points": 3.0 });
    let (status, answer) = call(&app, "POST", "/api/account/spam/words", Some(body), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let import = &answer["import"];
    assert_eq!(
        (import["added"].as_u64(), import["duplicates"].as_u64(), import["refusedCount"].as_u64()),
        (Some(2), Some(1), Some(1))
    );
    assert_eq!(import["refused"][0]["line"], "/(?=x)/");
    assert_eq!(answer["lists"]["entries"].as_array().map(Vec::len), Some(2));
    assert_eq!(answer["lists"]["defaultPoints"], 2.5);
    let body = json!({ "text": "roulette", "points": 20.0 });
    let (status, error) = call(&app, "POST", "/api/account/spam/words", Some(body), Some(&leni)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("wordInvalid")));

    for url in ["http://lists.example.org/bad.map", "https://127.0.0.1/bad.map", "https://localhost/bad.map"] {
        let (status, error) =
            call(&app, "POST", "/api/account/spam/word-sources", Some(json!({ "url": url })), Some(&leni)).await;
        assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("wordSourceInvalid")), "{url}");
    }
    // A link that leads nowhere is kept, with the reason, and fetched again later.
    let body = json!({ "url": "https://lists.invalid/bad.map", "subjectOnly": true });
    let (status, answer) = call(&app, "POST", "/api/account/spam/word-sources", Some(body), Some(&leni)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(answer["error"].is_string(), "{answer}");
    let source = &answer["lists"]["sources"][0];
    assert_eq!((source["subjectOnly"].as_bool(), source["error"].is_string()), (Some(true), true));

    let entry_id = answer["lists"]["entries"][0]["id"].as_i64().unwrap();
    let (status, _) =
        call(&app, "DELETE", &format!("/api/account/spam/words/{}", entry_id + 1000), None, Some(&leni)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nobody removes an entry that is not theirs");
    let (status, _) = call(&app, "DELETE", &format!("/api/admin/spam/words/{entry_id}"), None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "admins look after every list, people's too");
    let (status, _) = call(&app, "GET", "/api/admin/spam/words", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let body = json!({ "text": "lottery", "domain": "example.de" });
    let (status, answer) = call(&app, "POST", "/api/admin/spam/words", Some(body), Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let lists = &answer["lists"];
    assert_eq!(lists["entries"].as_array().map(Vec::len), Some(1), "Leni's entries are not the admins' business");
    assert_eq!(lists["entries"][0]["domain"], "example.de");
    assert_eq!(lists["domains"], json!(["example.de"]));

    let (status, feeds) = call(&app, "GET", "/api/admin/spam/feeds", None, Some(&chef)).await;
    assert_eq!(status, StatusCode::OK, "{feeds}");
    assert_eq!(feeds["feeds"].as_array().map(Vec::len), Some(6));
    let urlhaus = feeds["feeds"].as_array().unwrap().iter().find(|feed| feed["key"] == "urlhaus").unwrap();
    assert_eq!((urlhaus["needsKey"].as_bool(), urlhaus["active"].as_bool()), (Some(true), Some(false)));
    assert_eq!(feeds["abuseChKeySet"], false);
    let (status, error) = call(&app, "POST", "/api/admin/spam/feeds/urlhaus/refresh", None, Some(&chef)).await;
    assert_eq!((status, error["code"].as_str()), (StatusCode::CONFLICT, Some("feedInactive")));
    let (status, _) = call(&app, "GET", "/api/admin/spam/feeds", None, Some(&leni)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let actions: Vec<String> = store.audit_log(10, None).await.unwrap().into_iter().map(|entry| entry.action).collect();
    assert!(actions.contains(&"spam.wordsAdd".to_owned()), "{actions:?}");
}

#[tokio::test]
async fn rules_are_one_table_for_admins_and_each_person() {
    let (_dir, store, _ids) = server().await;
    let app = router(&store);
    let admin = login(&app, "chef@example.de").await;
    let leni = login(&app, "leni@example.de").await;

    let text = (0..40).map(|n| format!("spam{n}@evil.example")).collect::<Vec<_>>().join("\n");
    let import = json!({ "type": "sender", "list": "block", "text": text, "scope": "domain:example.de" });
    let (status, report) = call(&app, "POST", "/api/admin/spam/rules/import", Some(import), Some(&admin)).await;
    assert_eq!((status, report["added"].as_u64()), (StatusCode::OK, Some(40)), "{report}");
    let word = json!({ "type": "word", "value": "casino", "points": 4.0 });
    let (status, casino) = call(&app, "POST", "/api/admin/spam/rules", Some(word), Some(&admin)).await;
    assert_eq!(status, StatusCode::CREATED, "{casino}");
    assert_eq!((casino["list"].as_str(), casino["scope"]["type"].as_str()), (Some("points"), Some("server")));
    let own = json!({ "type": "sender", "list": "allow", "value": "oma@example.net", "scope": "domain:example.de" });
    let (status, oma) = call(&app, "POST", "/api/account/spam/rules", Some(own), Some(&leni)).await;
    assert_eq!(status, StatusCode::CREATED, "{oma}");
    assert_eq!(oma["scope"]["name"], "leni@example.de", "a person's rule is always their own");

    let (_, page) = call(&app, "GET", "/api/admin/spam/rules?perPage=25&page=1", None, Some(&admin)).await;
    assert_eq!((page["total"].as_i64(), page["rules"].as_array().unwrap().len()), (Some(42), 17), "{page}");
    let (_, page) = call(&app, "GET", "/api/admin/spam/rules?scope=accounts&list=allow", None, Some(&admin)).await;
    assert_eq!(page["total"], 1, "admins see people's rules too");
    let (_, page) = call(&app, "GET", "/api/account/spam/rules?scope=all", None, Some(&leni)).await;
    assert_eq!(page["total"], 1, "people only ever see their own");
    let (_, scopes) = call(&app, "GET", "/api/admin/spam/scopes?search=example", None, Some(&admin)).await;
    let domain = scopes["scopes"].as_array().unwrap().iter().find(|s| s["key"] == "domain:example.de").unwrap();
    assert_eq!(domain["count"], 40);

    let path = format!("/api/admin/spam/rules/word/{}", casino["id"]);
    let change = json!({ "points": null, "note": "Glücksspiel", "scope": "domain:example.de" });
    let (status, changed) = call(&app, "PATCH", &path, Some(change), Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!((changed["points"].clone(), changed["scope"]["name"].as_str()), (Value::Null, Some("example.de")));
    let theirs = path.replace("/admin/", "/account/");
    let stranger = call(&app, "PATCH", &theirs, Some(json!({ "note": "x" })), Some(&leni)).await;
    assert_eq!(stranger.0, StatusCode::NOT_FOUND, "people cannot reach the server's rules");

    let (_, page) = call(&app, "GET", "/api/admin/spam/rules?search=spam1&perPage=50", None, Some(&admin)).await;
    let items: Vec<Value> =
        page["rules"].as_array().unwrap().iter().map(|rule| json!({ "type": "sender", "id": rule["id"] })).collect();
    assert_eq!(items.len(), 11);
    let bulk = json!({ "items": items, "action": "expiry", "expiresAt": 4_000_000_000i64 });
    let (_, report) = call(&app, "POST", "/api/admin/spam/rules/bulk", Some(bulk), Some(&admin)).await;
    assert_eq!(report["changed"], 11, "{report}");
    let (_, page) = call(&app, "GET", "/api/admin/spam/rules?state=temporary", None, Some(&admin)).await;
    assert_eq!(page["total"], 11);
    let past = json!({ "items": [], "action": "expiry", "expiresAt": 1 });
    assert_eq!(
        call(&app, "POST", "/api/admin/spam/rules/bulk", Some(past), Some(&admin)).await.0,
        StatusCode::CONFLICT
    );
    let moved = json!({ "items": [{ "type": "sender", "id": oma["id"] }], "action": "scope", "scope": "server" });
    let (status, _) = call(&app, "POST", "/api/account/spam/rules/bulk", Some(moved), Some(&leni)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "people cannot move rules out of their own list");
    let delete = json!({ "items": items_of(&page), "action": "delete" });
    let (_, report) = call(&app, "POST", "/api/admin/spam/rules/bulk", Some(delete), Some(&admin)).await;
    assert_eq!(report["changed"], 11);

    let request = Request::get("/api/admin/spam/rules/export?list=allow")
        .header(header::COOKIE, &admin.0)
        .extension(ClientInfo { https: true, ..ClientInfo::default() })
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    let csv = String::from_utf8(axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    assert_eq!(csv.lines().count(), 2, "{csv}");
    assert!(csv.contains("sender,allow,address,oma@example.net,account:leni@example.de"), "{csv}");
}

fn items_of(page: &Value) -> Vec<Value> {
    page["rules"].as_array().unwrap().iter().map(|rule| json!({ "type": rule["type"], "id": rule["id"] })).collect()
}
