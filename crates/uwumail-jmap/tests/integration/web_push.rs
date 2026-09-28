//! Push subscriptions over Web Push (RFC 8620 7.2, RFC 8291, RFC 8292, RFC 9749): a push service
//! on this machine receives what the server sends, and the test decrypts it like a browser would.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, ECDH_P256, PrivateKey, UnparsedPublicKey};
use aws_lc_rs::hkdf::{HKDF_SHA256, KeyType, Salt};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey as SignatureKey};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use uwumail_jmap::{Jmap, PushMessage, PushTiming, PushTransport};
use uwumail_store::{AppScope, NewAppPassword};

use crate::common::{PASSWORD, Server, basic, server, smtp};

const CORE: &str = "urn:ietf:params:jmap:core";
const MAIL: &str = "urn:ietf:params:jmap:mail";
const MINI: &str = "mini@example.org";
const NYU: &str = "nyu@example.org";
/// Short waits, so the tests don't sit through the server's two and five seconds.
const QUICK: PushTiming = PushTiming { debounce: Duration::from_millis(50), min_interval: Duration::from_millis(150) };

/// What the push service got.
#[derive(Debug)]
struct Received {
    name: String,
    headers: HeaderMap,
    body: Bytes,
    at: Instant,
}

#[derive(Clone)]
struct Service {
    received: mpsc::UnboundedSender<Received>,
    /// The status each push name answers with; 201 when not set.
    statuses: Arc<Mutex<HashMap<String, u16>>>,
}

async fn receive(
    State(service): State<Service>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let status = service.statuses.lock().unwrap().get(&name).copied().unwrap_or(201);
    let _ = service.received.send(Received { name, headers, body, at: Instant::now() });
    StatusCode::from_u16(status).unwrap()
}

/// A push service on this machine: `http://127.0.0.1:<port>/push/<name>`.
async fn push_service() -> (String, mpsc::UnboundedReceiver<Received>, Arc<Mutex<HashMap<String, u16>>>) {
    let (sender, received) = mpsc::unbounded_channel();
    let statuses = Arc::new(Mutex::new(HashMap::new()));
    let service = Service { received: sender, statuses: statuses.clone() };
    let app = axum::Router::new().route("/push/{name}", post(receive)).with_state(service);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, received, statuses)
}

/// Plain HTTP to this machine: what the server's own way out never allows (that one is tested in
/// `uwumail-smtp`'s egress against a TLS server, and below for what it refuses).
struct Local;

impl PushTransport for Local {
    fn check_url(&self, url: &str) -> Result<(), String> {
        url.starts_with("http://127.0.0.1:").then_some(()).ok_or_else(|| "only the test's push service".into())
    }

    fn post(&self, message: PushMessage) -> Pin<Box<dyn Future<Output = Result<u16, String>> + Send + '_>> {
        Box::pin(async move {
            let rest = message.url.strip_prefix("http://").unwrap();
            let (host, path) = rest.split_once('/').unwrap();
            let mut stream = TcpStream::connect(host).await.map_err(|e| e.to_string())?;
            let mut head = format!("POST /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
            for (name, value) in &message.headers {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
            head.push_str(&format!("Content-Length: {}\r\n\r\n", message.body.len()));
            stream.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
            stream.write_all(&message.body).await.map_err(|e| e.to_string())?;
            let mut answer = Vec::new();
            stream.read_to_end(&mut answer).await.map_err(|e| e.to_string())?;
            let answer = String::from_utf8_lossy(&answer);
            answer.split(' ').nth(1).and_then(|status| status.parse().ok()).ok_or_else(|| answer.to_string())
        })
    }
}

/// A browser's side of a subscription: its key pair and auth secret.
struct Device {
    key: PrivateKey,
    auth: [u8; 16],
}

impl Device {
    fn new() -> Device {
        let mut auth = [0u8; 16];
        getrandom::fill(&mut auth).unwrap();
        Device { key: PrivateKey::generate(&ECDH_P256).unwrap(), auth }
    }

    fn keys(&self) -> Value {
        json!({ "p256dh": B64.encode(self.key.compute_public_key().unwrap().as_ref()), "auth": B64.encode(self.auth) })
    }

    /// RFC 8291 decryption, as a browser does it.
    fn decrypt(&self, message: &[u8]) -> Value {
        struct Len(usize);
        impl KeyType for Len {
            fn len(&self) -> usize {
                self.0
            }
        }
        let salt = &message[..16];
        assert_eq!(u32::from_be_bytes(message[16..20].try_into().unwrap()), 4096, "record size");
        let id_len = message[20] as usize;
        let sender = &message[21..21 + id_len];
        let record = &message[21 + id_len..];
        let own = self.key.compute_public_key().unwrap();
        let secret =
            agreement::agree(&self.key, UnparsedPublicKey::new(&ECDH_P256, sender), (), |s| Ok(s.to_vec())).unwrap();
        let mut ikm = [0u8; 32];
        Salt::new(HKDF_SHA256, &self.auth)
            .extract(&secret)
            .expand(&[b"WebPush: info\0", own.as_ref(), sender], Len(32))
            .unwrap()
            .fill(&mut ikm)
            .unwrap();
        let prk = Salt::new(HKDF_SHA256, salt).extract(&ikm);
        let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(&[b"Content-Encoding: aes128gcm\0"], Len(16)).unwrap().fill(&mut cek).unwrap();
        prk.expand(&[b"Content-Encoding: nonce\0"], Len(12)).unwrap().fill(&mut nonce).unwrap();
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).unwrap());
        let mut buffer = record.to_vec();
        let plain = key.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buffer).unwrap();
        let end = plain.iter().rposition(|byte| *byte != 0).unwrap();
        assert_eq!(plain[end], 0x02, "the last record's delimiter");
        serde_json::from_slice(&plain[..end]).unwrap()
    }
}

struct Setup {
    server: Server,
    base: String,
    received: mpsc::UnboundedReceiver<Received>,
    statuses: Arc<Mutex<HashMap<String, u16>>>,
    _shutdown: watch::Sender<bool>,
}

async fn setup() -> Setup {
    setup_with(QUICK).await
}

async fn setup_with(timing: PushTiming) -> Setup {
    let mut server = server().await;
    let jmap = Jmap::new(smtp(&server.store)).with_push_transport(Arc::new(Local)).with_push_timing(timing);
    server.router = jmap.router();
    server.jmap = jmap.clone();
    let (shutdown, shutdown_rx) = watch::channel(false);
    tokio::spawn(jmap.run_web_push(shutdown_rx));
    let (base, received, statuses) = push_service().await;
    Setup { server, base, received, statuses, _shutdown: shutdown }
}

/// A message as it comes from another server.
fn mail(subject: &str) -> String {
    format!("From: Nyu <nyu@example.net>\nTo: mini@example.org\nSubject: {subject}\n\nHallo\n")
}

impl Setup {
    async fn call(&self, login: &str, calls: Value) -> Vec<Value> {
        self.server.api_using(login, &[CORE, MAIL], calls).await
    }

    async fn call_as(&self, authorization: &str, calls: Value) -> Vec<Value> {
        let (status, response) = self.server.api_as(authorization, &[CORE, MAIL], calls).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        response["methodResponses"].as_array().unwrap().clone()
    }

    async fn next(&mut self) -> Received {
        tokio::time::timeout(Duration::from_secs(15), self.received.recv())
            .await
            .expect("a push within 15 seconds")
            .unwrap()
    }

    /// The next push to the subscription `name`, passing over pushes to others.
    async fn next_for(&mut self, name: &str) -> Received {
        loop {
            let received = self.next().await;
            if received.name == name {
                return received;
            }
        }
    }

    fn create(&self, name: &str, device: Option<&Device>, types: Value) -> Value {
        let mut create = json!({ "deviceClientId": name, "url": format!("{}/push/{name}", self.base), "types": types });
        if let Some(device) = device {
            create["keys"] = device.keys();
        }
        create
    }

    /// Creates a verified subscription with `authorization` and returns its id.
    async fn subscribe_as(&mut self, authorization: &str, name: &str, device: Option<&Device>, types: Value) -> String {
        let create = self.create(name, device, types);
        let responses =
            self.call_as(authorization, json!([["PushSubscription/set", { "create": { "k": create } }, "0"]])).await;
        let id =
            responses[0][1]["created"]["k"]["id"].as_str().unwrap_or_else(|| panic!("{}", responses[0])).to_owned();
        let verification = self.next_for(name).await;
        let body = match device {
            Some(device) => device.decrypt(&verification.body),
            None => serde_json::from_slice(&verification.body).unwrap(),
        };
        let update = json!({ id.clone(): { "verificationCode": body["verificationCode"] } });
        let responses = self.call_as(authorization, json!([["PushSubscription/set", { "update": update }, "0"]])).await;
        assert!(responses[0][1]["updated"].get(&id).is_some(), "{}", responses[0]);
        id
    }

    async fn subscribe(&mut self, login: &str, name: &str, device: Option<&Device>, types: Value) -> String {
        self.subscribe_as(&basic(login, PASSWORD), name, device, types).await
    }

    /// The ids of the subscriptions `authorization` lists.
    async fn listed(&self, authorization: &str) -> Vec<String> {
        let responses = self.call_as(authorization, json!([["PushSubscription/get", { "ids": null }, "0"]])).await;
        let list = responses[0][1]["list"].as_array().unwrap();
        list.iter().map(|s| s["id"].as_str().unwrap().to_owned()).collect()
    }

    /// Waits until the subscription `id`, made with `authorization`, is gone.
    async fn gone(&self, authorization: &str, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.listed(authorization).await.iter().any(|listed| listed == id) {
            assert!(Instant::now() < deadline, "subscription {id} is still there");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn header<'a>(received: &'a Received, name: &str) -> &'a str {
    received.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

#[tokio::test]
async fn a_subscription_is_verified_and_hears_of_new_mail() {
    let mut setup = setup().await;
    let account = setup.server.account_id(MINI).await;

    // The session names the key the browser binds its subscription to (RFC 9749).
    let session =
        Request::get("/jmap/session").header("authorization", basic(MINI, PASSWORD)).body(Body::empty()).unwrap();
    let (status, body) = setup.server.request(session).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_slice(&body).unwrap();
    let server_key = session["capabilities"]["urn:ietf:params:jmap:webpush-vapid"]["applicationServerKey"]
        .as_str()
        .unwrap()
        .to_owned();
    let raw_key = B64.decode(&server_key).unwrap();
    assert_eq!((raw_key.len(), raw_key[0]), (65, 0x04), "an uncompressed P-256 key");

    let device = Device::new();
    let url = format!("{}/push/browser", setup.base);
    let create = json!({ "deviceClientId": "browser-1", "url": url, "keys": device.keys(), "types": null });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "create": { "k": create } }, "0"]])).await;
    let set = &responses[0][1];
    assert!(set.get("accountId").is_none() && set.get("newState").is_none(), "{set}");
    let id = set["created"]["k"]["id"].as_str().unwrap().to_owned();
    assert!(set["created"]["k"]["expires"].is_string(), "the expiry the server chose: {set}");

    // The verification comes encrypted and signed.
    let verification = setup.next().await;
    assert_eq!(verification.name, "browser");
    assert_eq!(header(&verification, "content-encoding"), "aes128gcm");
    assert!(header(&verification, "ttl").parse::<u32>().unwrap() > 0);
    let authorization = header(&verification, "authorization");
    let (token, key) = authorization.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
    assert_eq!(key, server_key, "signed with the key the session shows");
    let (signed, signature) = token.rsplit_once('.').unwrap();
    SignatureKey::new(&ECDSA_P256_SHA256_FIXED, B64.decode(key).unwrap())
        .verify(signed.as_bytes(), &B64.decode(signature).unwrap())
        .unwrap();
    let (jwt_header, claims) = signed.split_once('.').unwrap();
    let jwt_header: Value = serde_json::from_slice(&B64.decode(jwt_header).unwrap()).unwrap();
    assert_eq!(jwt_header["alg"], "ES256");
    let claims: Value = serde_json::from_slice(&B64.decode(claims).unwrap()).unwrap();
    assert_eq!(claims["aud"], setup.base, "the push service's origin");
    assert_eq!(claims["sub"], "https://mail.example.org");
    let now = chrono::Utc::now().timestamp();
    let exp = claims["exp"].as_i64().unwrap();
    assert!(exp > now && exp <= now + 24 * 3600, "at most a day ahead (RFC 8292): {exp}");
    let body = device.decrypt(&verification.body);
    assert_eq!(body["@type"], "PushVerification");
    assert_eq!(body["pushSubscriptionId"], id);
    let code = body["verificationCode"].as_str().unwrap().to_owned();
    assert!(code.len() >= 32);

    // Until verified, the code is not shown; the address and keys never are.
    let responses = setup.call(MINI, json!([["PushSubscription/get", { "ids": null }, "0"]])).await;
    let got = &responses[0][1];
    assert!(got.get("accountId").is_none() && got.get("state").is_none(), "{got}");
    let listed = &got["list"][0];
    assert_eq!(listed["id"], id);
    assert_eq!(listed["deviceClientId"], "browser-1");
    assert_eq!(listed["verificationCode"], Value::Null);
    assert!(listed["expires"].is_string());
    assert!(listed.get("url").is_none() && listed.get("keys").is_none());
    let responses = setup.call(MINI, json!([["PushSubscription/get", { "properties": ["url"] }, "0"]])).await;
    assert_eq!(responses[0][0], "error");
    assert_eq!(responses[0][1]["type"], "forbidden");

    // Another person does not see it; a wrong code is refused.
    let responses = setup.call(NYU, json!([["PushSubscription/get", {}, "0"]])).await;
    assert_eq!(responses[0][1]["list"], json!([]));
    let wrong = json!({ id.clone(): { "verificationCode": "0000" } });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "update": wrong }, "0"]])).await;
    assert_eq!(responses[0][1]["notUpdated"][&id]["type"], "invalidProperties");
    assert_eq!(responses[0][1]["notUpdated"][&id]["properties"], json!(["verificationCode"]));

    let right = json!({ id.clone(): { "verificationCode": code } });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "update": right }, "0"]])).await;
    assert!(responses[0][1]["updated"].get(&id).is_some(), "{}", responses[0]);
    let responses = setup.call(MINI, json!([["PushSubscription/get", { "ids": [id] }, "0"]])).await;
    assert_eq!(responses[0][1]["list"][0]["verificationCode"], code);

    // New mail arrives: one StateChange, with the new state, high urgency.
    setup.server.deliver(MINI, &mail("Hello")).await;
    let new_state = setup.server.store.account_modseq(setup.server.id(MINI).await).await.unwrap().to_string();
    let pushed = setup.next().await;
    assert_eq!(header(&pushed, "urgency"), "high");
    assert!(!header(&pushed, "topic").is_empty());
    let change = device.decrypt(&pushed.body);
    assert_eq!(change["@type"], "StateChange");
    assert_eq!(change["changed"][&account]["Email"], new_state);
    assert_eq!(change["changed"][&account]["EmailDelivery"], new_state);
    assert!(change["changed"][&account]["Mailbox"].is_string(), "the inbox's counts changed too: {change}");
    assert!(pushed.body.windows(5).all(|w| w != b"Hello"), "no content leaves the server");

    // A draft saved is a change too, but no delivery.
    let drafts = setup.server.mailbox(MINI, "drafts").await;
    let draft = json!({ "mailboxIds": { drafts: true }, "keywords": { "$draft": true }, "subject": "Later",
                        "bodyValues": { "b": { "value": "Hi" } }, "textBody": [{ "partId": "b", "type": "text/plain" }] });
    setup.call(MINI, json!([["Email/set", { "accountId": account, "create": { "d": draft } }, "0"]])).await;
    let pushed = setup.next().await;
    let change = device.decrypt(&pushed.body);
    assert!(change["changed"][&account].get("Email").is_some(), "{change}");
    assert!(change["changed"][&account].get("EmailDelivery").is_none(), "{change}");
    assert_eq!(header(&pushed, "urgency"), "normal");

    // Destroyed, it is gone.
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "destroy": [id] }, "0"]])).await;
    assert_eq!(responses[0][1]["destroyed"], json!([id]));
    let responses = setup.call(MINI, json!([["PushSubscription/get", {}, "0"]])).await;
    assert_eq!(responses[0][1]["list"], json!([]));
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "destroy": [id] }, "0"]])).await;
    assert_eq!(responses[0][1]["notDestroyed"][&id]["type"], "notFound");
}

#[tokio::test]
async fn expiry_is_capped_renewable_and_immutable_properties_stay() {
    let setup = setup().await;
    let url = format!("{}/push/capped", setup.base);
    let far = "2099-01-01T00:00:00Z";
    let week = || chrono::Utc::now() + chrono::Duration::days(7) + chrono::Duration::seconds(5);
    let date = |value: &Value| chrono::DateTime::parse_from_rfc3339(value.as_str().unwrap()).unwrap();
    let create = json!({
        "far": { "deviceClientId": "d", "url": url, "expires": far },
        "none": { "deviceClientId": "d", "url": url },
        "past": { "deviceClientId": "d", "url": url, "expires": "2001-01-01T00:00:00Z" },
        "code": { "deviceClientId": "d", "url": url, "verificationCode": "mine" },
        "http": { "deviceClientId": "d", "url": "http://192.0.2.1/push" },
        "keys": { "deviceClientId": "d", "url": url, "keys": { "p256dh": "AAAA", "auth": "AAAA" } },
        "unknown": { "deviceClientId": "d", "url": url, "colour": "pink" },
        "nodevice": { "url": url },
    });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "create": create }, "0"]])).await;
    let set = &responses[0][1];
    for capped in ["far", "none"] {
        let at = date(&set["created"][capped]["expires"]);
        assert!(at < week() && at > chrono::Utc::now() + chrono::Duration::days(6), "{capped}: {at}");
    }
    assert_eq!(set["notCreated"]["past"]["properties"], json!(["expires"]));
    assert_eq!(set["notCreated"]["code"]["properties"], json!(["verificationCode"]));
    assert_eq!(set["notCreated"]["http"]["properties"], json!(["url"]));
    assert_eq!(set["notCreated"]["keys"]["properties"], json!(["keys"]));
    assert_eq!(set["notCreated"]["unknown"]["properties"], json!(["colour"]));
    assert_eq!(set["notCreated"]["nodevice"]["properties"], json!(["deviceClientId"]));

    // Renewing: a week at most, and the server says what it chose instead.
    let id = set["created"]["far"]["id"].as_str().unwrap().to_owned();
    let update = json!({ id.clone(): { "expires": far } });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "update": update }, "0"]])).await;
    let updated = date(&responses[0][1]["updated"][&id]["expires"]);
    assert!(updated < week(), "{updated}");
    // What fits is taken as it is.
    let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let update = json!({ id.clone(): { "expires": tomorrow } });
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "update": update }, "0"]])).await;
    assert_eq!(responses[0][1]["updated"][&id], Value::Null, "{}", responses[0]);
    let responses = setup.call(MINI, json!([["PushSubscription/get", { "ids": [id] }, "0"]])).await;
    assert_eq!(responses[0][1]["list"][0]["expires"], tomorrow);

    for (property, value) in [("url", json!(url)), ("deviceClientId", json!("e")), ("keys", Value::Null)] {
        let update = json!({ id.clone(): { property: value } });
        let responses = setup.call(MINI, json!([["PushSubscription/set", { "update": update }, "0"]])).await;
        assert_eq!(responses[0][1]["notUpdated"][&id]["properties"], json!([property]));
    }
    // Not someone else's.
    let update = json!({ id.clone(): { "expires": tomorrow } });
    let responses =
        setup.call(NYU, json!([["PushSubscription/set", { "update": update, "destroy": [id] }, "0"]])).await;
    assert_eq!(responses[0][1]["notUpdated"][&id]["type"], "notFound");
    assert_eq!(responses[0][1]["notDestroyed"][&id]["type"], "notFound");
}

#[tokio::test]
async fn subscriptions_belong_to_the_login_that_made_them_and_end_with_it() {
    let mut setup = setup().await;
    let mini = setup.server.id(MINI).await;
    let app = |name: &str| NewAppPassword { name: name.into(), scopes: vec![AppScope::Mail], expires_at: None };
    let phone = setup.server.store.create_app_password(mini, app("phone")).await.unwrap();
    let tablet = setup.server.store.create_app_password(mini, app("tablet")).await.unwrap();
    // The app password as a bearer token (docs/jmap-tokens.md) and in Basic, and the password.
    let as_phone = format!("Bearer {}", phone.secret);
    let as_tablet = basic(MINI, &tablet.secret);
    let as_password = basic(MINI, PASSWORD);

    let from_phone = setup.subscribe_as(&as_phone, "phone", None, json!(["EmailDelivery"])).await;
    let from_tablet = setup.subscribe_as(&as_tablet, "tablet", None, json!(["EmailDelivery"])).await;
    let from_password = setup.subscribe_as(&as_password, "password", None, json!(["EmailDelivery"])).await;
    assert_eq!(setup.listed(&as_phone).await, vec![from_phone.clone()]);
    assert_eq!(setup.listed(&as_tablet).await, vec![from_tablet.clone()]);
    assert_eq!(setup.listed(&as_password).await, vec![from_password.clone()]);
    // One login cannot touch another's.
    let responses =
        setup.call_as(&as_tablet, json!([["PushSubscription/set", { "destroy": [from_phone.clone()] }, "0"]])).await;
    assert_eq!(responses[0][1]["notDestroyed"][&from_phone]["type"], "notFound");

    // The phone's app password is removed: its subscription gets nothing more and is dropped at
    // once, not at the next clean-up.
    setup.server.store.revoke_app_password(mini, phone.app_password.id).await.unwrap();
    setup.server.deliver(MINI, &mail("After")).await;
    let mut names = vec![setup.next().await.name, setup.next().await.name];
    names.sort();
    assert_eq!(names, vec!["password", "tablet"]);
    assert_eq!(setup.server.store.purge_push_subscriptions().await.unwrap(), 0);
    assert_eq!(setup.listed(&as_tablet).await, vec![from_tablet]);
    assert_eq!(setup.listed(&as_password).await, vec![from_password]);
    // (A new password ends what the old one made: see the store's tests.)
}

/// What the webmail's service worker does (UwUMail-Webmail `src/push/worker.ts`): sign in with the
/// portal's session cookie and CSRF token, and ask what is new in the inbox.
#[tokio::test]
async fn the_webmail_subscribes_with_its_session_and_reads_what_came() {
    let mut setup = setup().await;
    let mini = setup.server.id(MINI).await;
    let account = setup.server.account_id(MINI).await;
    let session = setup.server.store.create_web_session(mini, 3600, "192.0.2.1", "test").await.unwrap();
    let cookie = format!("uwumail={}", session.token);
    let with_session = |calls: Value, csrf: Option<&str>| {
        let body = json!({ "using": [CORE, MAIL], "methodCalls": calls });
        let mut request = Request::post("/jmap/api")
            .header(header::COOKIE, cookie.clone())
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(csrf) = csrf {
            request = request.header("x-csrf-token", csrf);
        }
        request.body(Body::from(body.to_string())).unwrap()
    };
    let csrf = Some(session.csrf_token.as_str());

    // Without the CSRF token nothing happens.
    let device = Device::new();
    let create = setup.create("webmail", Some(&device), json!(["EmailDelivery"]));
    let calls = json!([["PushSubscription/set", { "create": { "push": create } }, "s"]]);
    let (status, _) = setup.server.request(with_session(calls.clone(), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = setup.server.request(with_session(calls, csrf)).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    let id = body["methodResponses"][0][1]["created"]["push"]["id"].as_str().unwrap().to_owned();
    let code = device.decrypt(&setup.next().await.body)["verificationCode"].as_str().unwrap().to_owned();
    let calls = json!([["PushSubscription/set", { "update": { &id: { "verificationCode": code } } }, "v"]]);
    let (status, _) = setup.server.request(with_session(calls, csrf)).await;
    assert_eq!(status, StatusCode::OK);
    // The password's login does not see the session's subscription.
    assert!(setup.listed(&basic(MINI, PASSWORD)).await.is_empty());

    // New mail: only EmailDelivery, as asked.
    let since = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    setup.server.deliver(MINI, &mail("Tea at five")).await;
    let change = device.decrypt(&setup.next().await.body);
    assert_eq!(
        change["changed"],
        json!({ &account: { "EmailDelivery": change["changed"][&account]["EmailDelivery"] } })
    );

    // The service worker asks the server what is new, with the cookie.
    let calls = json!([["Mailbox/query", { "accountId": account, "filter": { "role": "inbox" } }, "m"]]);
    let (status, body) = setup.server.request(with_session(calls, csrf)).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    let inbox = body["methodResponses"][0][1]["ids"][0].as_str().unwrap().to_owned();
    let calls = json!([
        ["Email/query", { "accountId": account,
                          "filter": { "inMailbox": inbox, "notKeyword": "$seen", "after": since },
                          "sort": [{ "property": "receivedAt", "isAscending": false }], "limit": 20 }, "q"],
        ["Email/get", { "accountId": account, "#ids": { "resultOf": "q", "name": "Email/query", "path": "/ids" },
                        "properties": ["id", "threadId", "from", "subject", "receivedAt"] }, "g"],
    ]);
    let (status, body) = setup.server.request(with_session(calls, csrf)).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    let list = &body["methodResponses"][1][1]["list"];
    assert_eq!(list.as_array().unwrap().len(), 1, "{body}");
    assert_eq!(list[0]["subject"], "Tea at five");
    assert_eq!(list[0]["from"][0]["email"], "nyu@example.net");

    // Signing out of the portal ends the subscription with the session.
    setup.server.store.delete_web_session(&session.token).await.unwrap();
    assert!(setup.server.store.push_targets(vec![mini]).await.unwrap().is_empty());
    assert_eq!(setup.server.store.purge_push_subscriptions().await.unwrap(), 1);
}

#[tokio::test]
async fn subscriptions_get_only_the_types_they_asked_for() {
    let mut setup = setup().await;
    let account = setup.server.account_id(MINI).await;
    setup.subscribe(MINI, "delivery", None, json!(["EmailDelivery"])).await;
    setup.subscribe(MINI, "folders", None, json!(["Mailbox"])).await;

    // A new folder: only the one that wants folders hears of it.
    let create = json!({ "f": { "name": "Receipts" } });
    setup.call(MINI, json!([["Mailbox/set", { "accountId": account, "create": create }, "0"]])).await;
    let pushed = setup.next().await;
    assert_eq!(pushed.name, "folders");
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    assert_eq!(change["changed"][&account].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["Mailbox"]);

    // New mail: the other one hears of it, with nothing but the delivery; whatever comes to it
    // before must say the same.
    setup.server.deliver(MINI, &mail("Hi")).await;
    let pushed = setup.next_for("delivery").await;
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    assert_eq!(change["changed"][&account].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["EmailDelivery"]);
}

#[tokio::test]
async fn people_a_folder_is_shared_with_hear_of_new_mail_in_it() {
    let mut setup = setup().await;
    let (mini, nyu) = (setup.server.id(MINI).await, setup.server.id(NYU).await);
    let inbox = setup.server.store.mailboxes(mini).await.unwrap();
    let inbox = inbox.iter().find(|m| m.role == Some(uwumail_store::MailboxRole::Inbox)).unwrap().id;
    setup.server.store.set_mailbox_acl_for(mini, inbox, nyu, "lr").await.unwrap();
    setup.subscribe(NYU, "nyu", None, json!(["EmailDelivery", "Email"])).await;

    setup.server.deliver(MINI, &mail("For both of us")).await;
    let pushed = setup.next_for("nyu").await;
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    let shared = &change["changed"][format!("a{mini}")];
    assert!(shared["EmailDelivery"].is_string() && shared["Email"].is_string(), "{change}");
    assert!(change["changed"].get(format!("a{nyu}")).is_none(), "nothing changed in nyu's own: {change}");
}

#[tokio::test]
async fn members_of_a_shared_mailbox_hear_of_new_mail_in_it() {
    let mut setup = setup().await;
    let support = setup
        .server
        .store
        .create_shared_mailbox(uwumail_store::NewSharedMailbox {
            address: "support@example.org".into(),
            name: "Support".into(),
            quota_bytes: 0,
            members: vec![(MINI.into(), true)],
        })
        .await
        .unwrap();
    let shared_account = format!("a{}", support.id);
    // The member's session has the shared mailbox as an account of its own, under that id.
    let responses = setup.call(MINI, json!([["Mailbox/get", { "accountId": shared_account, "ids": [] }, "0"]])).await;
    assert_eq!(responses[0][0], "Mailbox/get", "{}", responses[0]);
    setup.subscribe(MINI, "member", None, json!(["EmailDelivery"])).await;
    setup.subscribe(NYU, "outsider", None, json!(["EmailDelivery"])).await;

    setup.server.deliver(&support.login, &mail("Printer on fire")).await;
    let pushed = setup.next_for("member").await;
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    assert!(change["changed"][&shared_account]["EmailDelivery"].is_string(), "{change}");

    // Someone who is not a member hears nothing of it: the next push they get is their own mail.
    setup.server.deliver(NYU, &mail("Just for Nyu")).await;
    let pushed = setup.next_for("outsider").await;
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    let nyu = setup.server.account_id(NYU).await;
    assert_eq!(change["changed"].as_object().unwrap().keys().collect::<Vec<_>>(), vec![&nyu], "{change}");
}

/// Mail to an enabled masked address is new mail; a disabled one files it into the Trash, read,
/// and that is no news (docs/jmap-masked-email.md).
#[tokio::test]
async fn mail_to_a_disabled_masked_address_is_no_delivery() {
    const MASKED: [&str; 2] = [CORE, "https://www.fastmail.com/dev/maskedemail"];
    let mut setup = setup().await;
    let account = setup.server.account_id(MINI).await;
    let own = uwumail_store::DomainMaskedPolicy { mode: uwumail_store::MaskedMode::Own, ..Default::default() };
    setup.server.store.set_domain_masked_policy("example.org", own).await.unwrap();
    let responses = setup
        .server
        .api_using(
            MINI,
            &MASKED,
            json!([["MaskedEmail/set", { "accountId": account, "create": { "m": { "state": "enabled" } } }, "0"]]),
        )
        .await;
    let created = &responses[0][1]["created"]["m"];
    let (masked_id, address) =
        (created["id"].as_str().unwrap().to_owned(), created["email"].as_str().unwrap().to_owned());
    setup.subscribe(MINI, "masked", None, json!(["EmailDelivery", "Email"])).await;

    let smtp = smtp(&setup.server.store);
    let nyu = setup.server.store.account(NYU).await.unwrap().unwrap();
    let send = |subject: &str| uwumail_smtp::Submission {
        account: nyu.clone(),
        mail_from: NYU.into(),
        recipients: vec![uwumail_smtp::SubmissionRecipient { address: address.clone(), notify_flags: 0, orcpt: None }],
        raw: format!("From: Nyu <{NYU}>\r\nTo: <{address}>\r\nSubject: {subject}\r\n\r\nHallo\r\n").into_bytes(),
        env_id: None,
        trace: None,
    };
    smtp.submit(send("Your order")).await.unwrap();
    let change: Value = serde_json::from_slice(&setup.next_for("masked").await.body).unwrap();
    assert!(change["changed"][&account]["EmailDelivery"].is_string(), "{change}");

    let disable = json!({ &masked_id: { "state": "disabled" } });
    setup
        .server
        .api_using(MINI, &MASKED, json!([["MaskedEmail/set", { "accountId": account, "update": disable }, "0"]]))
        .await;
    smtp.submit(send("Another offer")).await.unwrap();
    let change: Value = serde_json::from_slice(&setup.next_for("masked").await.body).unwrap();
    assert!(change["changed"][&account]["Email"].is_string(), "{change}");
    assert!(change["changed"][&account].get("EmailDelivery").is_none(), "into the Trash: {change}");
}

#[tokio::test]
async fn changes_are_bundled_and_spaced_out() {
    let timing = PushTiming { debounce: Duration::from_millis(300), min_interval: Duration::from_millis(600) };
    let mut setup = setup_with(timing).await;
    let mini = setup.server.id(MINI).await;
    let account = setup.server.account_id(MINI).await;
    setup.subscribe(MINI, "bundled", None, json!(["Mailbox"])).await;

    // Three changes in a row, well within the wait: one push, with the last state.
    let create = json!({ "a": { "name": "One" }, "b": { "name": "Two" }, "c": { "name": "Three" } });
    let before = setup.server.store.account_modseq(mini).await.unwrap();
    let started = Instant::now();
    setup.call(MINI, json!([["Mailbox/set", { "accountId": account, "create": create }, "0"]])).await;
    let last = setup.server.store.account_modseq(mini).await.unwrap();
    assert!(last >= before + 3, "three changes, not one");
    let first = setup.next().await;
    let change: Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(change["changed"][&account]["Mailbox"], last.to_string());

    // The next change right away waits for the interval, and it is the very next push: nothing
    // more came of the three before.
    let create = json!({ "d": { "name": "Four" } });
    setup.call(MINI, json!([["Mailbox/set", { "accountId": account, "create": create }, "0"]])).await;
    let newest = setup.server.store.account_modseq(mini).await.unwrap();
    let second = setup.next().await;
    let change: Value = serde_json::from_slice(&second.body).unwrap();
    assert_eq!(change["changed"][&account]["Mailbox"], newest.to_string());
    // The first push leaves no sooner than `debounce` after the changes, and the second no sooner
    // than `min_interval` after the first left. When a push arrives says little about when it left,
    // so the bound counts from the changes: without the spacing the second would come after
    // hardly more than two debounces.
    assert!(second.at.duration_since(started) >= timing.debounce + timing.min_interval, "spaced out");
}

#[tokio::test]
async fn unencrypted_pushes_are_plain_json_and_forgotten_subscriptions_are_dropped() {
    let mut setup = setup().await;
    let account = setup.server.account_id(MINI).await;
    let id = setup.subscribe(MINI, "plain", None, json!(["Mailbox"])).await;

    let create = json!({ "f": { "name": "Receipts" } });
    setup.call(MINI, json!([["Mailbox/set", { "accountId": account, "create": create }, "0"]])).await;
    let pushed = setup.next().await;
    assert_eq!(pushed.name, "plain");
    assert!(pushed.headers.get("content-encoding").is_none());
    assert_eq!(header(&pushed, "content-type"), "application/json");
    let change: Value = serde_json::from_slice(&pushed.body).unwrap();
    assert!(change["changed"][&account].get("Mailbox").is_some(), "{change}");

    // The push service forgot the subscription (410): the server does too.
    setup.statuses.lock().unwrap().insert("plain".into(), 410);
    let create = json!({ "f": { "name": "Travel" } });
    setup.call(MINI, json!([["Mailbox/set", { "accountId": account, "create": create }, "0"]])).await;
    setup.next().await;
    setup.gone(&basic(MINI, PASSWORD), &id).await;

    // 404 as well, already for the verification.
    setup.statuses.lock().unwrap().insert("unknown".into(), 404);
    let create = setup.create("unknown", None, Value::Null);
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "create": { "k": create } }, "0"]])).await;
    let id = responses[0][1]["created"]["k"]["id"].as_str().unwrap().to_owned();
    assert_eq!(setup.next().await.name, "unknown");
    setup.gone(&basic(MINI, PASSWORD), &id).await;

    // Anything else is tried again later, not dropped.
    setup.statuses.lock().unwrap().insert("busy".into(), 503);
    let create = setup.create("busy", None, Value::Null);
    let responses = setup.call(MINI, json!([["PushSubscription/set", { "create": { "k": create } }, "0"]])).await;
    let id = responses[0][1]["created"]["k"]["id"].as_str().unwrap().to_owned();
    assert_eq!(setup.next().await.name, "busy");
    assert_eq!(setup.listed(&basic(MINI, PASSWORD)).await, vec![id]);
}

#[tokio::test]
async fn the_servers_own_way_out_takes_public_https_addresses_only() {
    let server = server().await;
    for url in [
        "http://push.example.net/a",
        "https://127.0.0.1/a",
        "https://[::1]/a",
        "https://10.0.0.1/a",
        "https://localhost/a",
        "https://push/a",
        "https://user:secret@push.example.net/a",
        "ftp://push.example.net/",
        "not a url",
    ] {
        let create = json!({ "k": { "deviceClientId": "d", "url": url } });
        let responses =
            server.api_using(MINI, &[CORE], json!([["PushSubscription/set", { "create": create }, "0"]])).await;
        assert_eq!(responses[0][1]["notCreated"]["k"]["properties"], json!(["url"]), "{url}");
    }
}
