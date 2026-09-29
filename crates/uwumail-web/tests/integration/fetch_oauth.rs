//! Fetched mailboxes that sign in at Microsoft and Google, through the portal's API: finding the
//! provider, starting a sign-in, Google's way back tied to the browser, and saving a proven sign-in
//! as a new mailbox or as the switch of one that logged in with a password. The providers' token
//! endpoints are a stand-in; nothing leaves this machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_jmap::ClientInfo;
use uwumail_smtp::autoconfig::{Login, Security, Server, Settings, Source};
use uwumail_smtp::provider_oauth::{BoxFuture, Endpoints, FetchOAuthConfig, FlowPoll, TokenTransport};
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{AfterFetch, FetchSecurity, NewAccount, NewFetchAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

/// The providers' token endpoints: every form is written down, and a fixed answer comes back.
#[derive(Default)]
struct Provider {
    seen: Mutex<Vec<(String, HashMap<String, String>)>>,
}

impl TokenTransport for Provider {
    fn post_form(&self, url: &str, form: String) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
        let fields: HashMap<String, String> = url::form_urlencoded::parse(form.as_bytes()).into_owned().collect();
        let answer = if url.ends_with("/devicecode") {
            json!({ "device_code": "dc", "user_code": "KX7PQ4M", "verification_uri": "https://microsoft.com/devicelogin",
                    "expires_in": 900, "interval": 5 })
        } else {
            json!({ "access_token": "at-google", "refresh_token": "rt-google", "expires_in": 3599 })
        };
        self.seen.lock().unwrap().push((url.to_owned(), fields));
        Box::pin(async move { Ok((200, answer.to_string().into_bytes())) })
    }
}

struct Portal {
    _dir: tempfile::TempDir,
    store: Store,
    smtp: Smtp,
    app: Router,
    auth: (String, String),
    person: i64,
}

async fn portal() -> Portal {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let person = store
        .create_account(NewAccount {
            address: "leni@example.org".into(),
            display_name: String::new(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap()
        .id;
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
        smtp.clone(),
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
    let app = web.router();
    let (_, login, _) = call(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({ "login": "leni@example.org", "password": "katzenpfote-123" })),
        None,
        None,
    )
    .await;
    let cookie = login["_cookie"].as_str().unwrap().to_owned();
    let auth = (cookie, login["csrfToken"].as_str().unwrap().to_owned());
    Portal { _dir: dir, store, smtp, app, auth, person }
}

/// One request; answers the status, the JSON (or the page, as a string) and the cookies set.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&(String, String)>,
    cookie: Option<&str>,
) -> (StatusCode, Value, Vec<String>) {
    let mut request = Request::builder().method(method).uri(path);
    let mut cookies: Vec<&str> = cookie.into_iter().collect();
    if let Some((session, csrf)) = auth {
        cookies.push(session);
        request = request.header(CSRF_HEADER, csrf);
    }
    if !cookies.is_empty() {
        request = request.header(header::COOKIE, cookies.join("; "));
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
    let set: Vec<String> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let mut value: Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    if let (Some(first), Value::Object(map)) = (set.first(), &mut value) {
        map.insert("_cookie".into(), first.clone().into());
    }
    (status, value, set)
}

/// What a real login to Google's servers would have proven; the proof itself is a login over the
/// internet, which the tests do not make.
fn google_proof() -> Settings {
    let server = |host: &str, port, security| Server { host: host.into(), port, security, login: Login::WholeAddress };
    Settings {
        imap: server("imap.gmail.com", 993, Security::Tls),
        smtp: Some(server("smtp.gmail.com", 465, Security::Tls)),
        source: Source::SignIn,
    }
}

/// Goes to Google and comes back: the flow the callback named, after the login proved it.
async fn signed_in_at_google(portal: &Portal, address: &str, switch_id: Option<i64>) -> String {
    let (status, started, set) = call(
        &portal.app,
        "POST",
        "/api/account/fetch/oauth/start",
        Some(json!({ "address": address, "provider": "google", "switchId": switch_id })),
        Some(&portal.auth),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let url = url::Url::parse(started["url"].as_str().unwrap()).unwrap();
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(query["redirect_uri"], "https://mail.example.org/api/account/fetch/oauth/callback");
    assert_eq!(query["login_hint"], address);
    let binding = set.iter().find(|cookie| cookie.starts_with("__Host-uwumail-fetch-oauth=")).expect("the binding");

    // Google sends the browser back: without the session cookie, which stays behind on a navigation
    // from another site, but with the binding.
    let back = format!("/api/account/fetch/oauth/callback?state={}&code=google-code", query["state"]);
    let (_, page, _) = call(&portal.app, "GET", &back, None, None, Some(binding)).await;
    let page = page.as_str().unwrap().to_owned();
    let flow = page.split("oauth=").nth(1).and_then(|rest| rest.split(['"', '&']).next()).expect("the flow");
    assert_eq!(flow, started["flowId"].as_str().unwrap());

    // The portal asks; the first answer is the one that proves it with a login.
    let oauth = portal.smtp.provider_oauth();
    assert!(matches!(oauth.poll(portal.person, flow).await, FlowPoll::Prove(_)));
    oauth.settle(portal.person, flow, Ok(google_proof()));
    let (_, status, _) =
        call(&portal.app, "GET", &format!("/api/account/fetch/oauth/flows/{flow}"), None, Some(&portal.auth), None)
            .await;
    assert_eq!(status["status"], "ready", "{status}");
    assert_eq!(status["settings"]["source"], "signIn");
    flow.to_owned()
}

#[tokio::test]
async fn microsoft_and_google_are_offered_and_the_device_code_is_handed_out() {
    let portal = portal().await;
    let provider = Arc::new(Provider::default());
    let oauth = portal.smtp.provider_oauth();
    oauth.set_transport(provider.clone());
    oauth.set_endpoints(Endpoints { microsoft: "https://login.test".into(), ..Endpoints::default() });

    let (_, view, _) = call(&portal.app, "GET", "/api/account/fetch", None, Some(&portal.auth), None).await;
    assert_eq!(view["signIn"]["microsoft"], true, "Microsoft works without anything set up");
    assert_eq!(view["signIn"]["google"], false, "Google needs the admin's own client");

    let detect = |address: &str| {
        let body = Some(json!({ "address": address }));
        let (app, auth) = (portal.app.clone(), portal.auth.clone());
        async move { call(&app, "POST", "/api/account/fetch/provider", body, Some(&auth), None).await.1 }
    };
    assert_eq!(detect("mini@hotmail.de").await, json!({ "provider": "microsoft", "ready": true }));
    assert_eq!(detect("mini@gmail.com").await, json!({ "provider": "google", "ready": false }));

    let start = |provider: &str| {
        let body = Some(json!({ "address": "Mini@Hotmail.de", "provider": provider }));
        let (app, auth) = (portal.app.clone(), portal.auth.clone());
        async move { call(&app, "POST", "/api/account/fetch/oauth/start", body, Some(&auth), None).await }
    };
    let (status, refused, _) = start("google").await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("oauthNotConfigured")), "{refused}");

    let (status, started, _) = start("microsoft").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    assert_eq!(started["device"]["userCode"], "KX7PQ4M");
    assert_eq!(started["device"]["verificationUri"], "https://microsoft.com/devicelogin");
    let (url, form) = provider.seen.lock().unwrap()[0].clone();
    assert_eq!(url, "https://login.test/consumers/oauth2/v2.0/devicecode", "Outlook.com and Hotmail are personal accounts");
    assert_eq!(form["client_id"], uwumail_smtp::provider_oauth::MICROSOFT_DEFAULT_CLIENT_ID);
    // Microsoft asked for five seconds between questions; the portal hears "not yet" meanwhile.
    let flow = started["device"]["flowId"].as_str().unwrap();
    let (_, status, _) =
        call(&portal.app, "GET", &format!("/api/account/fetch/oauth/flows/{flow}"), None, Some(&portal.auth), None)
            .await;
    assert_eq!(status["status"], "pending", "{status}");
    assert_eq!(provider.seen.lock().unwrap().len(), 1, "Microsoft is not asked before its interval");
    let (_, unknown, _) =
        call(&portal.app, "GET", "/api/account/fetch/oauth/flows/nothing-here", None, Some(&portal.auth), None).await;
    assert_eq!(unknown, json!({ "status": "failed", "error": "expired" }));
}

#[tokio::test]
async fn a_google_sign_in_is_tied_to_the_browser_and_saved_once() {
    let portal = portal().await;
    let provider = Arc::new(Provider::default());
    let oauth = portal.smtp.provider_oauth();
    oauth.set_transport(provider.clone());
    oauth.configure(FetchOAuthConfig {
        google_client_id: "gid.apps.googleusercontent.com".into(),
        google_client_secret: "g-secret".into(),
        ..Default::default()
    });

    // A way back from Google without the browser's binding does nothing.
    let (_, started, _) = call(
        &portal.app,
        "POST",
        "/api/account/fetch/oauth/start",
        Some(json!({ "address": "mini@gmail.com", "provider": "google" })),
        Some(&portal.auth),
        None,
    )
    .await;
    let url = url::Url::parse(started["url"].as_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(name, _)| name == "state").unwrap().1.into_owned();
    let back = format!("/api/account/fetch/oauth/callback?state={state}&code=stolen");
    let (_, page, _) =
        call(&portal.app, "GET", &back, None, None, Some("__Host-uwumail-fetch-oauth=someone-else")).await;
    assert!(page.as_str().unwrap().contains("oauthError=expired"), "{page}");
    let (_, page, _) = call(&portal.app, "GET", "/api/account/fetch/oauth/callback?error=access_denied", None, None, None).await;
    assert!(page.as_str().unwrap().contains("oauthError=declined"), "{page}");
    assert!(provider.seen.lock().unwrap().is_empty(), "Google is not asked for somebody else's code");

    let flow = signed_in_at_google(&portal, "mini@gmail.com", None).await;
    let (_, form) = provider.seen.lock().unwrap()[0].clone();
    assert_eq!((form["code"].as_str(), form["client_secret"].as_str()), ("google-code", "g-secret"));

    // A proven sign-in for one address does not save another.
    let (status, wrong, _) = call(
        &portal.app,
        "POST",
        "/api/account/fetch",
        Some(json!({ "address": "other@gmail.com", "oauthFlow": flow })),
        Some(&portal.auth),
        None,
    )
    .await;
    assert_eq!((status, wrong["code"].as_str()), (StatusCode::CONFLICT, Some("signInExpired")), "{wrong}");

    let flow = signed_in_at_google(&portal, "mini@gmail.com", None).await;
    let body = json!({ "address": "mini@gmail.com", "oauthFlow": flow, "afterFetch": "delete", "intervalSecs": 900 });
    let (status, created, _) =
        call(&portal.app, "POST", "/api/account/fetch", Some(body.clone()), Some(&portal.auth), None).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!((created["auth"].as_str(), created["host"].as_str()), (Some("google"), Some("imap.gmail.com")));
    assert_eq!((created["smtpHost"].as_str(), created["smtpPort"].as_u64()), (Some("smtp.gmail.com"), Some(465)));
    assert_eq!((created["afterFetch"].as_str(), created["intervalSecs"].as_i64()), (Some("delete"), Some(900)));
    assert_eq!(created["sendEnabled"], false, "answering from it still waits for one fetch");
    let id = created["id"].as_i64().unwrap();
    let kept = portal.store.fetch_oauth(portal.person, id).await.unwrap().unwrap();
    assert_eq!((kept.refresh_token.as_deref(), kept.access_token.as_deref()), (Some("rt-google"), Some("at-google")));
    let (status, again, _) = call(&portal.app, "POST", "/api/account/fetch", Some(body), Some(&portal.auth), None).await;
    assert_eq!((status, again["code"].as_str()), (StatusCode::CONFLICT, Some("signInExpired")), "only once: {again}");
}

#[tokio::test]
async fn a_mailbox_with_a_password_switches_to_signing_in() {
    let portal = portal().await;
    let oauth = portal.smtp.provider_oauth();
    oauth.set_transport(Arc::new(Provider::default()));
    oauth.configure(FetchOAuthConfig {
        google_client_id: "gid".into(),
        google_client_secret: "g-secret".into(),
        ..Default::default()
    });
    let fetched = portal
        .store
        .create_fetch_account(NewFetchAccount {
            account_id: portal.person,
            address: "mini@googlemail.com".into(),
            host: "imap.gmail.com".into(),
            port: 993,
            security: FetchSecurity::Tls,
            username: "mini@googlemail.com".into(),
            password: "app-password".into(),
            after_fetch: AfterFetch::MarkRead,
            fetch_junk: true,
            interval_secs: uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
            auth_serv_id: String::new(),
        })
        .await
        .unwrap();
    let (_, view, _) = call(&portal.app, "GET", "/api/account/fetch", None, Some(&portal.auth), None).await;
    assert_eq!(view["accounts"][0]["signIn"], "google", "the page offers the switch");
    assert_eq!(view["accounts"][0]["auth"], "password");

    // A sign-in started for a new mailbox does not switch this one.
    let loose = signed_in_at_google(&portal, "mini@googlemail.com", None).await;
    let (status, refused, _) = call(
        &portal.app,
        "PATCH",
        &format!("/api/account/fetch/{}", fetched.id),
        Some(json!({ "oauthFlow": loose })),
        Some(&portal.auth),
        None,
    )
    .await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("signInExpired")), "{refused}");

    let flow = signed_in_at_google(&portal, "mini@googlemail.com", Some(fetched.id)).await;
    let (status, switched, _) = call(
        &portal.app,
        "PATCH",
        &format!("/api/account/fetch/{}", fetched.id),
        Some(json!({ "oauthFlow": flow })),
        Some(&portal.auth),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(switched["auth"], "google");
    assert_eq!(switched["smtpHost"], "smtp.gmail.com");
    assert_eq!(portal.store.fetch_password(portal.person, fetched.id).await.unwrap().as_deref(), Some(""), "the password is forgotten");
    let (_, view, _) = call(&portal.app, "GET", "/api/account/fetch", None, Some(&portal.auth), None).await;
    assert_eq!(view["accounts"][0]["signIn"], Value::Null);

    // A password again switches it back.
    let (_, back, _) = call(
        &portal.app,
        "PATCH",
        &format!("/api/account/fetch/{}", fetched.id),
        Some(json!({ "password": "new-app-password" })),
        Some(&portal.auth),
        None,
    )
    .await;
    assert_eq!(back["auth"], "password");
    assert_eq!(portal.store.fetch_oauth(portal.person, fetched.id).await.unwrap().unwrap().refresh_token, None);
}
