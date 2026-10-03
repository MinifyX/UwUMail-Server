//! The admin's moves: checking a list, starting a domain move (domain, mailboxes and aliases made
//! on the way), password links, pausing, finishing, and that only admins get anywhere near it.
//! The copying itself is the server's worker, tested there; here nothing connects anywhere.

use std::sync::Arc;
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

const PASSWORD: &str = "katzenpfote-123";
const OLD_PASSWORD: &str = "altes-umzug-passwort";

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    body: Body,
    auth: Option<&(String, String)>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        request = request.header(header::COOKIE, &auth.0).header(CSRF_HEADER, &auth.1);
    }
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: &(String, String),
) -> (StatusCode, Value) {
    let body = body.map(|body| Body::from(body.to_string())).unwrap_or_else(|| Body::from("{}"));
    send(app, method, path, body, Some(auth)).await
}

async fn login(app: &Router, address: &str) -> (String, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": address, "password": PASSWORD }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie =
        response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

async fn portal() -> (Router, Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for (address, role) in [("admin@example.org", Role::Admin), ("mini@example.org", Role::User)] {
        let new = NewAccount {
            address: address.into(),
            display_name: String::new(),
            password: Some(PASSWORD.into()),
            role,
            quota_bytes: 0,
            protocols: None,
        };
        store.create_account(new).await.unwrap();
    }
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
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    (web.router(), store, dir)
}

fn domain_move(rows: Value) -> Value {
    json!({
        "kind": "domain",
        "domain": "umzug.example",
        "imapHost": "imap.example.net",
        "davMode": "sogo",
        "parallel": 2,
        "syncMinutes": 30,
        "rows": rows,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn only_admins_move() {
    let (app, _store, _dir) = portal().await;
    let mini = login(&app, "mini@example.org").await;
    let csv = json!({ "text": "a@example.org;pw" });
    for (method, path, body) in [
        ("GET", "/api/admin/moves", None),
        ("POST", "/api/admin/moves", Some(domain_move(json!([])))),
        ("POST", "/api/admin/moves/csv", Some(csv.clone())),
        ("POST", "/api/admin/moves/discover", Some(json!({ "address": "example.net" }))),
        ("GET", "/api/admin/moves/1", None),
        ("POST", "/api/admin/moves/1/finish", Some(json!({}))),
        ("POST", "/api/admin/moves/1/links", Some(json!({}))),
        ("POST", "/api/admin/moves/1/mailboxes/1/retry", Some(json!({}))),
        ("DELETE", "/api/admin/moves/1", None),
    ] {
        let (status, _) = call(&app, method, path, body, &mini).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let (status, _) = send(&app, "GET", "/api/admin/moves", Body::empty(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_domain_move_makes_what_is_missing_and_hands_out_links() {
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "admin@example.org").await;

    // A list from a spreadsheet, read into rows with its problems.
    let text = "Alte Adresse;Passwort;Name;Neue Adresse;Quota;Aliase\n\
                leni@umzug.example;pw-leni;Leni;;1 GB;info@umzug.example\n\
                nyu@umzug.example;pw-nyu;Nyu;;;\n\
                kaputt;;;;viel;\n";
    let (status, read) =
        call(&app, "POST", "/api/admin/moves/csv", Some(json!({ "text": text, "domain": "umzug.example" })), &admin)
            .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["rows"].as_array().unwrap().len(), 3);
    assert_eq!(read["rows"][0]["target"], "leni@umzug.example");
    assert_eq!(read["rows"][0]["quotaBytes"], 1024 * 1024 * 1024);
    let codes: Vec<&str> = read["problems"].as_array().unwrap().iter().map(|p| p["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["addressInvalid", "passwordMissing", "quotaInvalid"]);

    // Starting with a broken row changes nothing and names the row.
    let rows = json!([
        { "oldAddress": "leni@umzug.example", "password": OLD_PASSWORD, "name": "Leni", "quotaBytes": 1_073_741_824i64, "aliases": ["info@umzug.example"] },
        { "oldAddress": "nyu@umzug.example", "password": OLD_PASSWORD, "name": "Nyu" },
        { "oldAddress": "kaputt", "password": "" },
    ]);
    let (status, refused) = call(&app, "POST", "/api/admin/moves", Some(domain_move(rows.clone())), &admin).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("moveRows")), "{refused}");
    assert_eq!(refused["blockers"][0], json!({ "row": 2, "field": "oldAddress", "code": "addressInvalid" }));
    assert!(store.domain("umzug.example").await.unwrap().is_none(), "nothing was made");

    // The check before starting says what would happen.
    let good = json!([rows[0].clone(), rows[1].clone()]);
    let mut dry = domain_move(good.clone());
    dry["dryRun"] = json!(true);
    let (status, plan) = call(&app, "POST", "/api/admin/moves", Some(dry), &admin).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!((plan["domainExists"].as_bool(), plan["problems"].as_array().map(Vec::len)), (Some(false), Some(0)));
    assert_eq!(plan["rows"][0]["exists"], false);

    // Started: the domain with its DKIM keys, invited mailboxes, the alias, the move.
    let (status, started) = call(&app, "POST", "/api/admin/moves", Some(domain_move(good)), &admin).await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    assert!(!started.to_string().contains(OLD_PASSWORD), "passwords never come back");
    let id = started["move"]["id"].as_i64().unwrap();
    assert_eq!(
        (started["move"]["state"].as_str(), started["move"]["summary"]["queued"].as_i64()),
        (Some("active"), Some(2))
    );
    assert_eq!(started["move"]["davMode"], "sogo");
    assert!(!store.dkim_keys("umzug.example").await.unwrap().is_empty());
    let leni = store.person("leni@umzug.example").await.unwrap().unwrap();
    assert!(!leni.has_password && leni.account.display_name == "Leni");
    assert_eq!(leni.account.quota_bytes, 1_073_741_824);
    assert!(leni.addresses.iter().any(|a| a.address == "info@umzug.example" && a.kind == "alias"));
    let mailboxes = started["mailboxes"].as_array().unwrap();
    assert_eq!(mailboxes[0]["address"], "leni@umzug.example");
    assert_eq!(
        (mailboxes[0]["hasPortalPassword"].as_bool(), mailboxes[0]["createdAccount"].as_bool()),
        (Some(false), Some(true))
    );
    assert_eq!(mailboxes[0]["aliases"], json!(["info@umzug.example"]));

    // One mailbox at a time in a move: Leni cannot be in a second one.
    let single = json!({
        "kind": "mailbox", "domain": "umzug.example", "imapHost": "imap.example.net",
        "rows": [{ "oldAddress": "leni@example.net", "password": "x", "target": "leni@umzug.example" }],
    });
    let (status, busy) = call(&app, "POST", "/api/admin/moves", Some(single), &admin).await;
    assert_eq!((status, busy["blockers"][0]["code"].as_str()), (StatusCode::CONFLICT, Some("mailboxBusy")));

    // A single move into a mailbox that is there: filled, its person keeps the password.
    let fill = json!({
        "kind": "mailbox", "domain": "example.org", "imapHost": "imap.example.net", "davMode": "none",
        "rows": [{ "oldAddress": "mini@example.net", "password": OLD_PASSWORD, "target": "mini@example.org" }],
    });
    let (status, filled) = call(&app, "POST", "/api/admin/moves", Some(fill), &admin).await;
    assert_eq!(status, StatusCode::CREATED, "{filled}");
    let fill_id = filled["move"]["id"].as_i64().unwrap();
    assert_eq!(filled["mailboxes"][0]["hasPortalPassword"], true);
    let (_, fill_links) = call(&app, "POST", &format!("/api/admin/moves/{fill_id}/links"), None, &admin).await;
    assert_eq!(fill_links["links"].as_array().map(Vec::len), Some(0));
    assert_eq!(fill_links["skipped"][0]["reason"], "hasPassword");

    // Links for the invited ones; making them again replaces the ones before.
    let (status, links) = call(&app, "POST", &format!("/api/admin/moves/{id}/links"), None, &admin).await;
    assert_eq!(status, StatusCode::OK, "{links}");
    assert_eq!(links["links"].as_array().map(Vec::len), Some(2));
    let first = links["links"][0]["path"].as_str().unwrap().trim_start_matches("/password/").to_owned();
    assert!(store.password_link(&first).await.unwrap().is_some());
    let (_, again) = call(&app, "POST", &format!("/api/admin/moves/{id}/links"), None, &admin).await;
    assert_ne!(again["links"][0]["path"], links["links"][0]["path"]);
    assert!(store.password_link(&first).await.unwrap().is_none(), "the old link is replaced");

    // Pause, go on, slower; a mailbox is retried with a new password.
    let (status, paused) = call(&app, "POST", &format!("/api/admin/moves/{id}/pause"), None, &admin).await;
    assert_eq!((status, paused["move"]["state"].as_str()), (StatusCode::OK, Some("paused")));
    let (_, resumed) = call(&app, "POST", &format!("/api/admin/moves/{id}/resume"), None, &admin).await;
    assert_eq!(resumed["move"]["state"], "active");
    let (status, slower) = call(
        &app,
        "PATCH",
        &format!("/api/admin/moves/{id}"),
        Some(json!({ "parallel": 1, "syncMinutes": 120 })),
        &admin,
    )
    .await;
    assert_eq!((status, slower["move"]["parallel"].as_i64()), (StatusCode::OK, Some(1)));
    let (status, _) =
        call(&app, "PATCH", &format!("/api/admin/moves/{id}"), Some(json!({ "parallel": 99 })), &admin).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let leni_box = mailboxes[0]["id"].as_i64().unwrap();
    let (status, _) =
        call(&app, "POST", &format!("/api/admin/moves/{id}/mailboxes/{leni_box}/pause"), None, &admin).await;
    assert_eq!(status, StatusCode::OK);
    let retry = json!({ "password": "neues-passwort" });
    let (status, retried) =
        call(&app, "POST", &format!("/api/admin/moves/{id}/mailboxes/{leni_box}/retry"), Some(retry), &admin).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(store.move_mailbox_password(leni_box).await.unwrap().as_deref(), Some("neues-passwort"));
    // An id of another move's mailbox does not reach it.
    let (status, _) =
        call(&app, "POST", &format!("/api/admin/moves/{fill_id}/mailboxes/{leni_box}/retry"), None, &admin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Contacts as a file, for an old provider without CardDAV.
    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:kim@example.org\r\nFN:Kim\r\nEND:VCARD\r\n";
    let (status, uploaded) = send(
        &app,
        "POST",
        &format!("/api/admin/moves/{id}/mailboxes/{leni_box}/import?kind=addressbook"),
        Body::from(card),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{uploaded}");
    assert_eq!(uploaded["report"]["created"], 1);
    let (_, view) = call(&app, "GET", &format!("/api/admin/moves/{id}"), None, &admin).await;
    assert_eq!(view["mailboxes"][0]["contactsDone"], 1);

    // More people later, then finishing without a last round wipes every password at once.
    let more = json!({ "rows": [{ "oldAddress": "kim@umzug.example", "password": OLD_PASSWORD }] });
    let (status, grown) = call(&app, "POST", &format!("/api/admin/moves/{id}/mailboxes"), Some(more), &admin).await;
    assert_eq!((status, grown["move"]["summary"]["mailboxes"].as_i64()), (StatusCode::OK, Some(3)), "{grown}");
    let (status, finishing) = call(&app, "POST", &format!("/api/admin/moves/{id}/finish"), None, &admin).await;
    assert_eq!((status, finishing["move"]["state"].as_str()), (StatusCode::OK, Some("finishing")));
    let skip = json!({ "skipLastRound": true });
    let (_, done) = call(&app, "POST", &format!("/api/admin/moves/{id}/finish"), Some(skip), &admin).await;
    assert_eq!(done["move"]["state"], "done");
    assert!(done["mailboxes"].as_array().unwrap().iter().all(|m| m["hasPassword"] == false));
    assert_eq!(store.move_mailbox_password(leni_box).await.unwrap(), None);

    // The change log has it all.
    let log = store.audit_log(100, None).await.unwrap();
    let actions: Vec<&str> = log.iter().map(|entry| entry.action.as_str()).collect();
    for action in ["move.create", "domain.create", "account.create", "alias.add", "move.finish", "move.upload"] {
        assert!(actions.contains(&action), "{action} in {actions:?}");
    }
    let (status, _) = call(&app, "DELETE", &format!("/api/admin/moves/{id}"), None, &admin).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(store.person("leni@umzug.example").await.unwrap().is_some(), "the mailboxes stay");
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_that_clash_are_named() {
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "admin@example.org").await;
    store.add_alias("kontakt@example.org", "mini@example.org").await.unwrap();
    let mut body = json!({
        "kind": "domain", "domain": "example.org", "imapHost": "imap.example.net", "dryRun": true,
        "rows": [
            { "oldAddress": "a@example.net", "password": "x", "target": "kontakt@example.org" },
            { "oldAddress": "b@example.net", "password": "x", "target": "b@example.org", "aliases": ["kontakt@example.org"] },
            { "oldAddress": "c@example.net", "password": "x", "target": "c@example.com" },
            { "oldAddress": "d@example.net", "password": "x", "target": "d@example.org", "aliases": ["x@nowhere.example"] },
            { "oldAddress": "e@example.net", "password": "x", "target": "b@example.org", "imapHost": "10.1.2.3" },
        ],
    });
    let (status, plan) = call(&app, "POST", "/api/admin/moves", Some(body.clone()), &admin).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    let problems: Vec<(i64, &str)> = plan["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["row"].as_i64().unwrap(), p["code"].as_str().unwrap()))
        .collect();
    assert_eq!(
        problems,
        [
            (0, "targetTaken"),
            (1, "aliasTaken"),
            (2, "targetInvalid"),
            (3, "aliasDomain"),
            (4, "duplicate"),
            (4, "hostInvalid")
        ]
    );
    body["dryRun"] = json!(false);
    body["imapHost"] = json!("192.168.1.1");
    let (status, refused) = call(&app, "POST", "/api/admin/moves", Some(body), &admin).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("hostInvalid")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_move_leaves_nothing_behind() {
    use uwumail_store::{NewGroup, NewMigrationJob, NewMove, NewMoveMailbox, WhoMaySend};
    let (app, store, _dir) = portal().await;
    let admin = login(&app, "admin@example.org").await;
    let rows = |alias: &str| {
        json!([
            { "oldAddress": "leni@umzug.example", "password": OLD_PASSWORD, "name": "Leni" },
            { "oldAddress": "nyu@umzug.example", "password": OLD_PASSWORD, "aliases": [alias] },
        ])
    };

    // An alias that is a group's address fails only when it is added, after Leni's mailbox and
    // the domain were made: both are taken back (security review 0.22 MOV-3).
    let group = NewGroup {
        address: "team@example.org".into(),
        name: "Team".into(),
        who_may_send: WhoMaySend::Anyone,
        members_may_send_as: false,
        members: vec!["mini@example.org".into()],
    };
    store.create_group(group).await.unwrap();
    let (status, refused) =
        call(&app, "POST", "/api/admin/moves", Some(domain_move(rows("team@example.org"))), &admin).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(store.account("leni@umzug.example").await.unwrap().is_none(), "the mailbox made on the way is gone");
    assert!(store.account("nyu@umzug.example").await.unwrap().is_none());
    assert!(store.domain("umzug.example").await.unwrap().is_none(), "the domain made on the way is gone");
    assert!(store.moves().await.unwrap().is_empty());

    // A mailbox whose person moves mail in themselves is refused before anything is made.
    let mini = store.account("mini@example.org").await.unwrap().unwrap();
    let job = NewMigrationJob {
        account_id: mini.id,
        address: "mini@example.net".into(),
        host: "imap.example.net".into(),
        port: 993,
        login: "mini@example.net".into(),
        password: OLD_PASSWORD.into(),
    };
    store.create_migration_job(job).await.unwrap();
    let single = json!({
        "kind": "mailbox",
        "domain": "example.org",
        "imapHost": "imap.example.net",
        "davMode": "auto",
        "rows": [{ "oldAddress": "mini@example.net", "password": OLD_PASSWORD, "target": "mini@example.org" }],
    });
    let (_, refused) = call(&app, "POST", "/api/admin/moves", Some(single), &admin).await;
    assert_eq!(refused["code"], "movePersonalBusy", "{refused}");

    // At the limit of open moves, a new domain is not even made.
    for i in 0..uwumail_store::MAX_OPEN_MOVES {
        let new = NewAccount {
            address: format!("p{i}@example.org"),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let id = store.create_account(new).await.unwrap().id;
        let open = NewMove {
            kind: uwumail_store::MoveKind::Mailbox,
            domain: "example.org".into(),
            imap_host: "imap.example.net".into(),
            imap_port: 993,
            dav_mode: uwumail_store::DavMode::Auto,
            dav_host: String::new(),
            dav_url: String::new(),
            contacts: false,
            calendars: false,
            parallel: 1,
            sync_minutes: 60,
            created_by: None,
        };
        let mailbox = NewMoveMailbox {
            account_id: id,
            old_address: format!("p{i}@example.net"),
            login: String::new(),
            password: OLD_PASSWORD.into(),
            imap_host: None,
            imap_port: None,
            dav_url: String::new(),
            created_account: false,
        };
        store.create_move(open, vec![mailbox]).await.unwrap();
    }
    let (_, refused) =
        call(&app, "POST", "/api/admin/moves", Some(domain_move(rows("info@umzug.example"))), &admin).await;
    assert_eq!(refused["code"], "movesLimit", "{refused}");
    assert!(store.domain("umzug.example").await.unwrap().is_none(), "nothing was made");
    assert!(store.account("leni@umzug.example").await.unwrap().is_none());
}
