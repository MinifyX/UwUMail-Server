//! Groups, shared mailboxes and masked addresses through the portal API (docs/groups.md,
//! docs/jmap-masked-email.md).

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

type Auth = (String, String);

async fn portal() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, name, role) in [
        ("nyu@example.org", "Nyu", Role::Admin),
        ("mini@example.org", "Mini", Role::User),
        ("leni@example.org", "Leni", Role::User),
    ] {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: name.into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
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
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: None,
            certificate: None,
            webmail: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    (web.router(), store, dir)
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>, auth: &Auth) -> (StatusCode, Value) {
    let mut request =
        Request::builder().method(method).uri(path).header(header::COOKIE, &auth.0).header(CSRF_HEADER, &auth.1);
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn login(app: &Router, address: &str) -> Auth {
    let body = json!({ "login": address, "password": "katzenpfote-123" });
    let mut request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn admins_keep_groups_of_a_domain() {
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "nyu@example.org").await;
    let mini = login(&app, "mini@example.org").await;

    let new = json!({ "local": "Vorstand", "name": "Der Vorstand", "whoMaySend": "members",
                      "membersMaySendAs": true, "members": ["mini@example.org", "leni@example.org"] });
    let (status, refused) = call(&app, "POST", "/api/admin/domains/example.org/groups", Some(new.clone()), &mini).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    let (status, group) = call(&app, "POST", "/api/admin/domains/example.org/groups", Some(new.clone()), &admin).await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    assert_eq!(group["address"], "vorstand@example.org");
    assert_eq!(group["whoMaySend"], "members");
    assert_eq!(group["members"].as_array().unwrap().len(), 2);
    let (status, _) = call(&app, "POST", "/api/admin/domains/example.org/groups", Some(new), &admin).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let taken = json!({ "local": "mini", "members": [] });
    let (status, _) = call(&app, "POST", "/api/admin/domains/example.org/groups", Some(taken), &admin).await;
    assert_eq!(status, StatusCode::CONFLICT, "a person's address is no group's");
    let bad = json!({ "local": "info", "whoMaySend": "everybody" });
    let (status, _) = call(&app, "POST", "/api/admin/domains/example.org/groups", Some(bad), &admin).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The domain shows it, and keeps it in use.
    let (_, domain) = call(&app, "GET", "/api/admin/domains/example.org", None, &admin).await;
    assert_eq!(domain["groups"][0]["name"], "Der Vorstand");
    assert_eq!(domain["maskedAddresses"], false);

    // Members see it in their addresses.
    let (_, addresses) = call(&app, "GET", "/api/account/addresses", None, &mini).await;
    assert_eq!(
        addresses["groups"],
        json!([{ "address": "vorstand@example.org", "name": "Der Vorstand", "maySendAs": true }])
    );

    let change = json!({ "members": ["leni@example.org"], "whoMaySend": "anyone" });
    let (status, group) =
        call(&app, "PATCH", "/api/admin/domains/example.org/groups/vorstand", Some(change), &admin).await;
    assert_eq!(status, StatusCode::OK, "{group}");
    assert_eq!(
        (group["whoMaySend"].as_str(), group["members"][0]["login"].as_str()),
        (Some("anyone"), Some("leni@example.org"))
    );
    let (_, addresses) = call(&app, "GET", "/api/account/addresses", None, &mini).await;
    assert_eq!(addresses["groups"], json!([]));

    let (status, _) = call(&app, "DELETE", "/api/admin/domains/example.org/groups/vorstand", None, &admin).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&app, "DELETE", "/api/admin/domains/example.org/groups/vorstand", None, &admin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(store.groups(None).await.unwrap().is_empty());

    let (_, log) = call(&app, "GET", "/api/admin/audit?limit=5", None, &admin).await;
    let actions: Vec<&str> = log.as_array().unwrap().iter().filter_map(|entry| entry["action"].as_str()).collect();
    assert_eq!(actions[..3], ["group.remove", "group.update", "group.create"]);
}

#[tokio::test]
async fn admins_make_shared_mailboxes() {
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "nyu@example.org").await;
    let mini = login(&app, "mini@example.org").await;

    let new = json!({ "address": "Support@example.org", "name": "Support", "quotaBytes": 1_000_000,
                      "members": [{ "login": "mini@example.org", "maySend": true }, { "login": "leni@example.org" }] });
    let (status, created) = call(&app, "POST", "/api/admin/shared-mailboxes", Some(new), &admin).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["person"]["login"], "support@example.org");
    assert_eq!(created["person"]["sharedMailbox"], true);
    assert_eq!(created["members"].as_array().unwrap().len(), 2);

    let (_, detail) = call(&app, "GET", "/api/admin/people/support@example.org", None, &admin).await;
    let members: Vec<(&str, bool)> = detail["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["login"].as_str().unwrap(), m["maySend"].as_bool().unwrap()))
        .collect();
    assert_eq!(members, [("leni@example.org", false), ("mini@example.org", true)]);
    let (_, list) = call(&app, "GET", "/api/admin/shared-mailboxes", None, &admin).await;
    assert_eq!(list[0]["login"], "support@example.org");

    // Nobody signs in to it, and it stays a shared mailbox.
    let password = json!({ "password": "ein-langes-passwort-123" });
    let (status, refused) =
        call(&app, "PUT", "/api/admin/people/support@example.org/password", Some(password), &admin).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("sharedMailbox")));

    // Members see it; a change of members shows at once.
    let (_, addresses) = call(&app, "GET", "/api/account/addresses", None, &mini).await;
    assert_eq!(addresses["sharedMailboxes"][0]["address"], "support@example.org");
    assert_eq!(addresses["sharedMailboxes"][0]["maySend"], true);
    let only_leni = json!({ "members": [{ "login": "leni@example.org", "maySend": true }] });
    let (status, members) =
        call(&app, "PUT", "/api/admin/shared-mailboxes/support@example.org/members", Some(only_leni), &admin).await;
    assert_eq!((status, members.as_array().map(Vec::len)), (StatusCode::OK, Some(1)));
    let (_, addresses) = call(&app, "GET", "/api/account/addresses", None, &mini).await;
    assert_eq!(addresses["sharedMailboxes"], json!([]));
    let leni = store.account("leni@example.org").await.unwrap().unwrap();
    assert!(store.account_owns_address(leni.id, "support@example.org").await.unwrap());

    // Only people are members, and only admins decide.
    let service = json!({ "members": [{ "login": "support@example.org" }] });
    let (status, _) =
        call(&app, "PUT", "/api/admin/shared-mailboxes/support@example.org/members", Some(service), &admin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "GET", "/api/admin/shared-mailboxes", None, &mini).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "PUT",
        "/api/admin/shared-mailboxes/mini@example.org/members",
        Some(json!({ "members": [] })),
        &admin,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a person is no shared mailbox");
}

#[tokio::test]
async fn people_make_masked_addresses_where_a_domain_allows() {
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "nyu@example.org").await;
    let mini = login(&app, "mini@example.org").await;

    let (_, view) = call(&app, "GET", "/api/account/masked", None, &mini).await;
    assert_eq!(view, json!({ "addresses": [], "domains": [] }));
    let new = json!({ "description": "Bäckerei", "forDomain": "https://baeckerei.example.com", "emailPrefix": "brot" });
    let (status, refused) = call(&app, "POST", "/api/account/masked", Some(new.clone()), &mini).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("maskedDomain")));

    let (status, _) =
        call(&app, "PUT", "/api/admin/domains/example.org/masked-addresses", Some(json!({ "on": true })), &mini).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) =
        call(&app, "PUT", "/api/admin/domains/example.org/masked-addresses", Some(json!({ "on": true })), &admin).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Made by hand, it is on right away.
    let (status, created) = call(&app, "POST", "/api/account/masked", Some(new), &mini).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let email = created["email"].as_str().unwrap().to_owned();
    assert!(email.starts_with("brot.") && email.ends_with("@example.org"), "{email}");
    assert_eq!((created["state"].as_str(), created["createdBy"].as_str()), (Some("enabled"), Some("Portal")));
    let id = created["id"].as_i64().unwrap();
    let bad = json!({ "emailPrefix": "brot und butter" });
    let (status, refused) = call(&app, "POST", "/api/account/masked", Some(bad), &mini).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("maskedPrefix")));

    // The domain is in use as long as it takes mail.
    let (_, domain) = call(&app, "GET", "/api/admin/domains/example.org", None, &admin).await;
    assert_eq!((domain["maskedAddresses"].as_bool(), domain["maskedInUse"].as_i64()), (Some(true), Some(1)));

    let path = format!("/api/account/masked/{id}");
    let (status, changed) =
        call(&app, "PATCH", &path, Some(json!({ "state": "disabled", "description": "Brötchen" })), &mini).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!((changed["state"].as_str(), changed["description"].as_str()), (Some("disabled"), Some("Brötchen")));
    let (status, _) = call(&app, "PATCH", &path, Some(json!({ "state": "pending" })), &mini).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nobody else's to change.
    let leni = login(&app, "leni@example.org").await;
    let (status, _) = call(&app, "PATCH", &path, Some(json!({ "state": "enabled" })), &leni).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &path, None, &leni).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, view) = call(&app, "DELETE", &path, None, &mini).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["addresses"][0]["state"], "deleted");
    assert_eq!(store.resolve_recipient(&email).await.unwrap(), None);
    let (_, domain) = call(&app, "GET", "/api/admin/domains/example.org", None, &admin).await;
    assert_eq!(domain["maskedInUse"], 0);
}
