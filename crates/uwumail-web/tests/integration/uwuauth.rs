//! Pairing with UwUAuth and SCIM (docs/uwuauth.md), against a small UwUAuth in this process. The
//! transport hands requests to its router instead of the network: the real one only trusts public
//! certificates, which no test server has.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::client::{Answer, BoxFuture, RemoteError, Transport};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{AccountUpdate, NewAccount, Role, Store};
use uwumail_web::settings::{SETTINGS, SettingSource, SettingValue, SettingsBackend, get_path};
use uwumail_web::{AuthConfig, CSRF_HEADER, Web, WebSettings};

const ISSUER: &str = "https://auth.example.net";
const CODE: &str = "7KQ4-M2XD-9HFT";
const SCIM_TOKEN: &str = "scim-token-0123456789abcdef0123456789abcdef";

/// What the fake UwUAuth saw.
#[derive(Default)]
struct Seen {
    /// The app of the pairing that worked.
    app: Option<Value>,
    code_used: bool,
}

async fn server() -> impl IntoResponse {
    axum::Json(json!({
        "product": "UwUAuth", "version": "0.5.0", "name": "UwUAuth at home", "issuer": ISSUER,
        "protocols": ["oidc", "scim"], "openidConfiguration": format!("{ISSUER}/.well-known/openid-configuration"),
        "pairing": 1, "pair": format!("{ISSUER}/uwu/v1/pair"), "scim": true,
    }))
}

async fn discovery() -> impl IntoResponse {
    axum::Json(json!({
        "issuer": ISSUER,
        "authorization_endpoint": format!("{ISSUER}/authorize"),
        "token_endpoint": format!("{ISSUER}/token"),
        "jwks_uri": format!("{ISSUER}/jwks"),
        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
    }))
}

async fn pair(State(seen): State<Arc<Mutex<Seen>>>, body: Bytes) -> impl IntoResponse {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let mut seen = seen.lock().unwrap();
    match body["code"].as_str() {
        Some("SLOW-DOWN") => return (StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({ "error": "too_many" }))),
        Some(CODE) if !seen.code_used => {}
        _ => return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_code" }))),
    }
    seen.code_used = true;
    seen.app = Some(body["app"].clone());
    (
        StatusCode::OK,
        axum::Json(json!({
            "issuer": ISSUER, "clientId": "uwumail-1", "clientSecret": "client-secret-of-the-pairing",
            "tokenEndpointAuthMethod": "client_secret_basic", "scopes": ["openid", "email", "profile", "roles"],
            "groupsClaim": "groups", "rolesClaim": "roles", "scimToken": SCIM_TOKEN, "appId": "app-1",
            "manageUrl": format!("{ISSUER}/admin/apps/app-1"),
        })),
    )
}

/// "The network": UwUAuth at its host and at a second name (which it does not call its issuer),
/// nothing anywhere else.
struct Network(Router);

impl Transport for Network {
    fn send(&self, request: Request<Bytes>, max_bytes: usize) -> BoxFuture<'_, Result<Answer, RemoteError>> {
        Box::pin(async move {
            let (mut parts, body) = request.into_parts();
            let host = parts.uri.host().map(str::to_owned);
            parts.uri = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").parse().unwrap();
            let response = match host.as_deref() {
                Some("auth.example.net" | "lan.example.net") => {
                    self.0.clone().oneshot(Request::from_parts(parts, Body::from(body))).await.unwrap()
                }
                _ => StatusCode::NOT_FOUND.into_response(),
            };
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body =
                axum::body::to_bytes(response.into_body(), max_bytes).await.map_err(|_| RemoteError::TooLarge)?;
            Ok(Answer { status, headers, body })
        })
    }
}

/// Settings that are applied to logging in elsewhere, the way the server does it.
#[derive(Default)]
struct FakeServer {
    web: OnceLock<Web>,
}

impl SettingsBackend for FakeServer {
    fn view(&self, overlay: &Value) -> Result<Vec<SettingValue>, String> {
        Ok(SETTINGS
            .iter()
            .map(|spec| {
                let value = get_path(overlay, spec.key).cloned().unwrap_or(Value::Null);
                let source = if value.is_null() { SettingSource::Default } else { SettingSource::Database };
                SettingValue { key: spec.key, set: !value.is_null(), value, source }
            })
            .collect())
    }

    fn apply(&self, overlay: &Value) -> Result<(), String> {
        let config = self.auth_config(overlay)?;
        self.web.get().expect("set up").external_login().configure(config);
        Ok(())
    }

    fn config_file(&self) -> Option<String> {
        None
    }

    fn auth_config(&self, overlay: &Value) -> Result<AuthConfig, String> {
        let auth = overlay.get("auth").cloned().unwrap_or(json!({}));
        serde_json::from_value(auth).map_err(|err| err.to_string())
    }
}

struct Setup {
    app: Router,
    web: Web,
    store: Store,
    seen: Arc<Mutex<Seen>>,
    auth: (String, String),
    _dir: tempfile::TempDir,
}

async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    store
        .create_account(NewAccount {
            address: "nyu@example.org".into(),
            display_name: "Nyu".into(),
            password: Some("katzenpfote-123".into()),
            role: Role::Admin,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let settings = SmtpSettings {
        hostname: "mail.example.org".into(),
        smtp: Default::default(),
        spam: Default::default(),
        delivery: Default::default(),
        tone: Default::default(),
        server_tls: None,
    };
    let backend = Arc::new(FakeServer::default());
    let web = Web::new(
        Smtp::new(store.clone(), settings).unwrap(),
        WebSettings {
            hostname: "mail.example.org".into(),
            started: Instant::now(),
            logs: None,
            loki: None,
            config: Some(backend.clone()),
            certificate: None,
            webmail: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        },
    );
    backend.web.set(web.clone()).ok();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let uwuauth = Router::new()
        .route("/uwu/v1/server", get(server))
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/uwu/v1/pair", post(pair))
        .with_state(seen.clone());
    web.set_oidc_transport(Arc::new(Network(uwuauth)));
    let app = web.router();
    let auth = login(&app).await;
    Setup { app, web, store, seen, auth, _dir: dir }
}

async fn login(app: &Router) -> (String, String) {
    let mut request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": "nyu@example.org", "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let csrf = serde_json::from_slice::<Value>(&bytes).unwrap()["csrfToken"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

async fn admin(setup: &Setup, method: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri("/api/admin/uwuauth")
        .header(header::COOKIE, &setup.auth.0)
        .header(CSRF_HEADER, &setup.auth.1);
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = setup.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn scim(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: &str,
) -> (StatusCode, Value, HeaderMap) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/scim/v2{path}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/scim+json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    // Not a loopback address: the limiter would never count one.
    let ip = "203.0.113.7".parse().unwrap();
    request.extensions_mut().insert(ClientInfo { ip, https: true });
    let response = app.clone().oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
}

async fn paired() -> Setup {
    let setup = setup().await;
    let (status, view) = admin(&setup, "POST", Some(json!({ "code": format!("{ISSUER}/#pair={CODE}") }))).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    setup
}

fn filter(attribute: &str, value: &str) -> String {
    let filter = format!("{attribute} eq \"{value}\"");
    format!("?filter={}", url::form_urlencoded::byte_serialize(filter.as_bytes()).collect::<String>())
}

#[tokio::test]
async fn pairing_fills_in_the_login_and_unpairing_empties_it() {
    let setup = setup().await;
    let (status, view) = admin(&setup, "GET", None).await;
    assert_eq!((status, view["paired"].as_bool()), (StatusCode::OK, Some(false)), "{view}");
    assert_eq!(view["scimUrl"], "https://mail.example.org/scim/v2");

    // What does not pair.
    let wrong = [
        (json!({ "url": ISSUER, "code": "WRONG-CODE" }), "pairingCodeInvalid"),
        (json!({ "url": ISSUER, "code": "SLOW-DOWN" }), "pairingTooMany"),
        (json!({ "url": "https://other.example.net", "code": CODE }), "notUwuauth"),
        (json!({ "url": "http://auth.example.net", "code": CODE }), "uwuauthAddress"),
    ];
    for (body, code) in wrong {
        let (status, answer) = admin(&setup, "POST", Some(body.clone())).await;
        assert_eq!((status, answer["code"].as_str()), (StatusCode::CONFLICT, Some(code)), "{body}: {answer}");
    }
    let (status, _) = admin(&setup, "POST", Some(json!({ "code": CODE }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "an address is needed");
    assert!(!setup.web.external_login().config().oidc.enabled);

    // The text of the QR code is enough.
    let (status, view) = admin(&setup, "POST", Some(json!({ "code": format!(" {ISSUER}/#pair={CODE} ") }))).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!((view["paired"].as_bool(), view["loginActive"].as_bool()), (Some(true), Some(true)), "{view}");
    assert_eq!(view["pairing"]["issuer"], ISSUER);
    assert_eq!(view["pairing"]["manageUrl"], format!("{ISSUER}/admin/apps/app-1"));
    assert!(!view.to_string().contains(SCIM_TOKEN) && !view.to_string().contains("client-secret"), "{view}");

    let app = setup.seen.lock().unwrap().app.clone().expect("UwUAuth was asked");
    assert_eq!(app["product"], "UwUMail");
    assert_eq!(app["redirectUris"], json!(["https://mail.example.org/api/auth/oidc/callback"]));
    assert_eq!(app["scim"]["baseUrl"], "https://mail.example.org/scim/v2");
    assert_eq!(app["roles"][0]["id"], "admin");

    let config = setup.web.external_login().config();
    let oidc = &config.oidc;
    assert!(oidc.enabled && oidc.sync_admin && !oidc.auto_create && !oidc.private_network);
    assert_eq!((oidc.issuer.as_str(), oidc.client_id.as_str()), (ISSUER, "uwumail-1"));
    assert_eq!(oidc.client_secret, "client-secret-of-the-pairing");
    assert_eq!((oidc.admin_group_claim.as_str(), oidc.admin_group_value.as_str()), ("roles", "admin"));
    assert_eq!(oidc.scopes, ["openid", "email", "profile", "roles"]);

    // The token works; a used code does not pair again.
    let (status, _, _) = scim(&setup.app, "GET", "/ServiceProviderConfig", None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    let (_, answer) = admin(&setup, "POST", Some(json!({ "url": ISSUER, "code": CODE }))).await;
    assert_eq!(answer["code"], "pairingCodeInvalid");

    let (status, _) = admin(&setup, "DELETE", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!setup.web.external_login().config().oidc.enabled);
    assert!(setup.web.external_login().config().oidc.client_secret.is_empty());
    let (_, view) = admin(&setup, "GET", None).await;
    assert_eq!(view["paired"], false);
    let (status, _, _) = scim(&setup.app, "GET", "/ServiceProviderConfig", None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the token went with the pairing");
    let (status, _) = admin(&setup, "DELETE", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn scim_needs_the_token_and_says_what_it_can() {
    let setup = paired().await;
    let (status, error, headers) = scim(&setup.app, "GET", "/Users", None, "not-the-token").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error["schemas"][0], "urn:ietf:params:scim:api:messages:2.0:Error");
    assert_eq!(error["status"], "401");
    assert_eq!(headers[header::CONTENT_TYPE], "application/scim+json");
    let (status, _, _) = scim(&setup.app, "GET", "/Users", None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, config, _) = scim(&setup.app, "GET", "/ServiceProviderConfig", None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (config["patch"]["supported"].as_bool(), config["bulk"]["supported"].as_bool()),
        (Some(true), Some(false))
    );
    for path in ["/ResourceTypes", "/Schemas"] {
        let (status, list, _) = scim(&setup.app, "GET", path, None, SCIM_TOKEN).await;
        assert_eq!((status, list["totalResults"].as_u64()), (StatusCode::OK, Some(2)), "{path}");
    }

    // Filters other than `eq` are refused the SCIM way.
    for bad in ["userName co \"a\"", "nickName eq \"a\"", "userName eq a"] {
        let query = format!("?filter={}", url::form_urlencoded::byte_serialize(bad.as_bytes()).collect::<String>());
        let (status, error, _) = scim(&setup.app, "GET", &format!("/Users{query}"), None, SCIM_TOKEN).await;
        assert_eq!((status, error["scimType"].as_str()), (StatusCode::BAD_REQUEST, Some("invalidFilter")), "{bad}");
    }
}

#[tokio::test]
async fn people_become_mailboxes_and_go_to_the_trash() {
    let setup = paired().await;
    let app = &setup.app;
    let lena = json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": "Lena@example.org", "externalId": "sub-lena", "displayName": "Lena",
        "active": true, "roles": [{ "value": "user" }],
    });
    let (status, user, headers) = scim(app, "POST", "/Users", Some(lena.clone()), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    let id = user["id"].as_str().unwrap().to_owned();
    assert_eq!(headers[header::LOCATION], format!("https://mail.example.org/scim/v2/Users/{id}"));
    assert_eq!((user["userName"].as_str(), user["active"].as_bool()), (Some("lena@example.org"), Some(true)));
    assert!(user["meta"]["created"].as_str().unwrap().ends_with('Z'));
    let account = setup.store.account("lena@example.org").await.unwrap().expect("a mailbox");
    assert_eq!((account.display_name.as_str(), account.role), ("Lena", Role::User));
    assert_eq!(setup.store.auth_source(account.id).await.unwrap(), "oidc");
    assert!(!setup.store.has_password(account.id).await.unwrap());
    // The login at UwUAuth belongs to the mailbox from the start: `sub` is the external id.
    assert_eq!(setup.store.external_identity(ISSUER, "sub-lena").await.unwrap(), Some(account.id));

    // Again: 409, and UwUAuth finds the person by filter.
    let (status, error, _) = scim(app, "POST", "/Users", Some(lena), SCIM_TOKEN).await;
    assert_eq!((status, error["scimType"].as_str()), (StatusCode::CONFLICT, Some("uniqueness")));
    for (attribute, value) in [("userName", "lena@example.org"), ("externalId", "sub-lena"), ("id", id.as_str())] {
        let (status, list, _) =
            scim(app, "GET", &format!("/Users{}", filter(attribute, value)), None, SCIM_TOKEN).await;
        assert_eq!((status, list["totalResults"].as_u64()), (StatusCode::OK, Some(1)), "{attribute}");
        assert_eq!(list["Resources"][0]["id"], id.as_str());
    }
    let (_, list, _) = scim(app, "GET", "/Users?startIndex=1&count=1", None, SCIM_TOKEN).await;
    assert_eq!((list["totalResults"].as_u64(), list["itemsPerPage"].as_u64()), (Some(2), Some(1)), "{list}");

    // Only on mail domains of this server.
    let stranger = json!({ "userName": "someone@elsewhere.example.com", "externalId": "sub-x" });
    let (status, error, _) = scim(app, "POST", "/Users", Some(stranger), SCIM_TOKEN).await;
    assert_eq!((status, error["scimType"].as_str()), (StatusCode::BAD_REQUEST, Some("invalidValue")));
    let (status, _, _) = scim(app, "POST", "/Users", Some(json!({ "userName": "no address" })), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let taken = json!({ "userName": "other@example.org", "externalId": "sub-lena" });
    let (status, _, _) = scim(app, "POST", "/Users", Some(taken), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CONFLICT, "the external id is Lena's");
    assert!(setup.store.account("other@example.org").await.unwrap().is_none());

    // Locked and unlocked by UwUAuth.
    let path = format!("/Users/{id}");
    let active = |on: bool| json!({ "Operations": [{ "op": "replace", "path": "active", "value": on }] });
    let (status, user, _) = scim(app, "PATCH", &path, Some(active(false)), SCIM_TOKEN).await;
    assert_eq!((status, user["active"].as_bool()), (StatusCode::OK, Some(false)), "{user}");
    assert!(setup.store.account("lena@example.org").await.unwrap().unwrap().disabled);
    let (_, user, _) = scim(app, "PATCH", &path, Some(active(true)), SCIM_TOKEN).await;
    assert_eq!(user["active"], true);
    // What an admin locked stays locked.
    let locked = AccountUpdate { disabled: Some(true), ..Default::default() };
    setup.store.update_account("lena@example.org", locked).await.unwrap();
    let (_, user, _) = scim(app, "PATCH", &path, Some(active(true)), SCIM_TOKEN).await;
    assert_eq!(user["active"], false, "only SCIM's own lock is lifted");
    let unlocked = AccountUpdate { disabled: Some(false), ..Default::default() };
    setup.store.update_account("lena@example.org", unlocked).await.unwrap();

    // Names, roles, and the address that stays.
    let change = json!({ "Operations": [
        { "op": "replace", "value": { "displayName": "Lena Lindwurm", "active": "True" } },
        { "op": "replace", "path": "roles", "value": [{ "value": "admin" }] },
    ] });
    let (status, user, _) = scim(app, "PATCH", &path, Some(change), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!(
        (user["displayName"].as_str(), user["roles"][0]["value"].as_str()),
        (Some("Lena Lindwurm"), Some("admin"))
    );
    assert_eq!(setup.store.account("lena@example.org").await.unwrap().unwrap().role, Role::Admin);
    let moved = json!({ "Operations": [{ "op": "replace", "path": "userName", "value": "lena2@example.org" }] });
    let (status, error, _) = scim(app, "PATCH", &path, Some(moved), SCIM_TOKEN).await;
    assert_eq!((status, error["scimType"].as_str()), (StatusCode::BAD_REQUEST, Some("mutability")));
    let put = json!({ "userName": "lena@example.org", "displayName": "Lena", "externalId": "sub-lena", "roles": [] });
    let (status, user, _) = scim(app, "PUT", &path, Some(put), SCIM_TOKEN).await;
    assert_eq!((status, user["displayName"].as_str()), (StatusCode::OK, Some("Lena")), "{user}");
    assert_eq!(setup.store.account("lena@example.org").await.unwrap().unwrap().role, Role::User);
    let (status, _, _) = scim(app, "PATCH", &path, Some(json!({ "op": "replace" })), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a PATCH without Operations");

    // The admin with a password is not UwUAuth's to lock.
    let nyu = setup.store.account("nyu@example.org").await.unwrap().unwrap();
    let (_, list, _) =
        scim(app, "GET", &format!("/Users{}", filter("userName", "nyu@example.org")), None, SCIM_TOKEN).await;
    let nyu_id = list["Resources"][0]["id"].as_str().unwrap().to_owned();
    let (status, user, _) = scim(app, "PATCH", &format!("/Users/{nyu_id}"), Some(active(false)), SCIM_TOKEN).await;
    assert_eq!((status, user["active"].as_bool()), (StatusCode::OK, Some(true)), "{user}");
    assert!(!setup.store.account_by_id(nyu.id).await.unwrap().unwrap().disabled);

    // Deleted: in the trash, where an admin can still restore it.
    let (status, _, _) = scim(app, "DELETE", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(setup.store.account_by_id(account.id).await.unwrap().unwrap().deleted_at.is_some());
    let (status, _, _) = scim(app, "GET", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, error, _) = scim(app, "PATCH", &path, Some(active(true)), SCIM_TOKEN).await;
    assert_eq!((status, error["status"].as_str()), (StatusCode::NOT_FOUND, Some("404")));
}

#[tokio::test]
async fn groups_are_kept_for_the_provider() {
    let setup = paired().await;
    let app = &setup.app;
    let staff =
        json!({ "displayName": "Staff", "externalId": "g-1", "members": [{ "value": "u-1" }, { "value": "u-2" }] });
    let (status, group, headers) = scim(app, "POST", "/Groups", Some(staff.clone()), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    let id = group["id"].as_str().unwrap().to_owned();
    assert_eq!(headers[header::LOCATION], format!("https://mail.example.org/scim/v2/Groups/{id}"));
    let (status, error, _) = scim(app, "POST", "/Groups", Some(staff), SCIM_TOKEN).await;
    assert_eq!((status, error["scimType"].as_str()), (StatusCode::CONFLICT, Some("uniqueness")));
    let (_, list, _) = scim(app, "GET", &format!("/Groups{}", filter("displayName", "Staff")), None, SCIM_TOKEN).await;
    assert_eq!((list["totalResults"].as_u64(), list["Resources"][0]["id"].as_str()), (Some(1), Some(id.as_str())));

    let path = format!("/Groups/{id}");
    let change = json!({ "Operations": [
        { "op": "add", "path": "members", "value": [{ "value": "u-3" }] },
        { "op": "remove", "path": "members[value eq \"u-1\"]" },
        { "op": "replace", "path": "displayName", "value": "Team" },
    ] });
    let (status, group, _) = scim(app, "PATCH", &path, Some(change), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK, "{group}");
    assert_eq!(group["displayName"], "Team");
    assert_eq!(group["members"], json!([{ "value": "u-2" }, { "value": "u-3" }]));
    let (status, group, _) =
        scim(app, "PUT", &path, Some(json!({ "displayName": "Team", "members": [] })), SCIM_TOKEN).await;
    assert_eq!((status, group["members"].clone()), (StatusCode::OK, json!([])));
    assert_eq!(group["externalId"], Value::Null);

    let (status, _, _) = scim(app, "DELETE", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = scim(app, "GET", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = scim(app, "DELETE", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn accounts_from_before_are_noted_but_not_taken_over() {
    let setup = paired().await;
    let app = &setup.app;
    let mini = setup
        .store
        .create_account(NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: Some("schnurrhaar-456".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();

    // How UwUAuth adopts: create, 409, find by userName, PATCH everything with the externalId.
    let person = json!({ "userName": "mini@example.org", "externalId": "sub-mini", "displayName": "Someone" });
    let (status, _, _) = scim(app, "POST", "/Users", Some(person), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, list, _) =
        scim(app, "GET", &format!("/Users{}", filter("userName", " MINI@Example.ORG ")), None, SCIM_TOKEN).await;
    assert_eq!(list["totalResults"], 1, "the userName is matched in its normal form: {list}");
    let id = list["Resources"][0]["id"].as_str().unwrap().to_owned();
    let path = format!("/Users/{id}");
    let adopt = json!({ "Operations": [
        { "op": "replace", "value": { "active": false, "displayName": "Someone", "externalId": "sub-mini" } },
        { "op": "replace", "path": "roles", "value": [{ "value": "admin" }] },
    ] });
    let (status, user, _) = scim(app, "PATCH", &path, Some(adopt), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!((user["externalId"].as_str(), user["active"].as_bool()), (Some("sub-mini"), Some(true)), "{user}");

    // Noted, but no login link: that waits for a login with a verified email.
    assert_eq!(setup.store.external_identity(ISSUER, "sub-mini").await.unwrap(), None);
    let account = setup.store.account_by_id(mini.id).await.unwrap().unwrap();
    assert_eq!((account.disabled, account.role, account.display_name.as_str()), (false, Role::User, "Mini"));
    assert!(setup.store.has_password(mini.id).await.unwrap());
    let put = json!({ "userName": "mini@example.org", "active": false, "externalId": "sub-mini", "roles": ["admin"] });
    let (status, _, _) = scim(app, "PUT", &path, Some(put), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::OK);
    let account = setup.store.account_by_id(mini.id).await.unwrap().unwrap();
    assert_eq!((account.disabled, account.role), (false, Role::User));

    // A DELETE lets go of the account; the mailbox stays.
    let (status, _, _) = scim(app, "DELETE", &path, None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(setup.store.account_by_id(mini.id).await.unwrap().unwrap().deleted_at.is_none());
    let (_, list, _) = scim(app, "GET", &format!("/Users{}", filter("externalId", "sub-mini")), None, SCIM_TOKEN).await;
    assert_eq!(list["totalResults"], 0, "{list}");
}

#[tokio::test]
async fn pages_far_out_are_empty() {
    let setup = paired().await;
    for path in ["/Users", "/Groups"] {
        let query = format!("{path}?startIndex={}&count={}", usize::MAX, usize::MAX);
        let (status, list, _) = scim(&setup.app, "GET", &query, None, SCIM_TOKEN).await;
        assert_eq!(status, StatusCode::OK, "{path}: {list}");
        assert_eq!(list["Resources"], json!([]), "{path}");
    }
}

#[tokio::test]
async fn refused_tokens_have_a_budget_of_their_own() {
    let setup = paired().await;
    let mut refused = 0;
    loop {
        let (status, _, _) = scim(&setup.app, "GET", "/Users", None, "not-the-token").await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        refused += 1;
        assert!(refused < 100, "no end to the tries");
    }
    let (status, _, _) = scim(&setup.app, "GET", "/Users", None, SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "the network waits, whatever it sends");
    // Passwords are counted apart: the admin can still log in from the same network.
    let mut request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "login": "nyu@example.org", "password": "katzenpfote-123" }).to_string()))
        .unwrap();
    request.extensions_mut().insert(ClientInfo { ip: "203.0.113.7".parse().unwrap(), https: true });
    assert_eq!(setup.app.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn in_the_local_network_the_issuer_is_the_host_typed() {
    let setup = setup().await;
    // lan.example.net answers, but names auth.example.net as the issuer.
    let body = json!({ "url": "https://lan.example.net", "code": CODE, "privateNetwork": true });
    let (status, answer) = admin(&setup, "POST", Some(body)).await;
    assert_eq!((status, answer["code"].as_str()), (StatusCode::CONFLICT, Some("uwuauthAddress")), "{answer}");
    assert!(!setup.seen.lock().unwrap().code_used, "the code was not sent");

    let body = json!({ "url": ISSUER, "code": CODE, "privateNetwork": true });
    let (status, view) = admin(&setup, "POST", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(setup.web.external_login().config().oidc.private_network);
}

#[tokio::test]
async fn another_uwuauth_forgets_what_the_last_one_said() {
    let setup = paired().await;
    let person = json!({ "userName": "lena@example.org", "externalId": "sub-lena", "active": false });
    let (status, _, _) = scim(&setup.app, "POST", "/Users", Some(person), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = scim(&setup.app, "POST", "/Groups", Some(json!({ "displayName": "Staff" })), SCIM_TOKEN).await;
    assert_eq!(status, StatusCode::CREATED);

    // Paired before with another UwUAuth.
    let stored = setup.store.setting("uwuauth.pairing").await.unwrap().unwrap();
    let mut pairing: Value = serde_json::from_str(&stored).unwrap();
    pairing["issuer"] = json!("https://old.example.net");
    setup.store.set_setting("uwuauth.pairing", &pairing.to_string()).await.unwrap();
    setup.seen.lock().unwrap().code_used = false;
    let (status, view) = admin(&setup, "POST", Some(json!({ "url": ISSUER, "code": CODE }))).await;
    assert_eq!(status, StatusCode::OK, "{view}");

    let (_, list, _) =
        scim(&setup.app, "GET", &format!("/Users{}", filter("externalId", "sub-lena")), None, SCIM_TOKEN).await;
    assert_eq!(list["totalResults"], 0, "{list}");
    let (_, groups, _) = scim(&setup.app, "GET", "/Groups", None, SCIM_TOKEN).await;
    assert_eq!(groups["totalResults"], 0, "{groups}");
}

#[tokio::test]
async fn a_failed_create_leaves_no_account() {
    let setup = paired().await;
    let lena = setup
        .store
        .scim_create_account(
            NewAccount {
                address: "lena@example.org".into(),
                display_name: "Lena".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            },
            Some("sub-lena".into()),
            true,
        )
        .await
        .unwrap();
    assert!(lena.disabled_by_scim && lena.account.disabled);
    assert_eq!(setup.store.auth_source(lena.account.id).await.unwrap(), "oidc");
    let again = NewAccount {
        address: "other@example.org".into(),
        display_name: String::new(),
        password: None,
        role: Role::User,
        quota_bytes: 0,
        protocols: None,
    };
    assert!(setup.store.scim_create_account(again, Some("sub-lena".into()), false).await.is_err());
    assert!(setup.store.account("other@example.org").await.unwrap().is_none());
}
