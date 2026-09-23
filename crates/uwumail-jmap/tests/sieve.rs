//! Mail rules over JMAP (RFC 9661) and what delivery makes of them: scripts go up as blobs, are
//! checked, activated, and then sort mail that arrives over SMTP.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tower::ServiceExt;
use uwumail_jmap::Jmap;
use uwumail_smtp::{DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig};
use uwumail_store::{EmailSummary, NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const SIEVE: &str = "urn:ietf:params:jmap:sieve";
const USING: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", SIEVE];

struct Server {
    router: Router,
    store: Store,
    mx: SocketAddr,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.com").await.unwrap();
    for user in ["mini", "nyu"] {
        store
            .create_account(NewAccount {
                address: format!("{user}@example.com"),
                display_name: user.to_uppercase(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.com".into(),
            smtp: SmtpConfig::default(),
            spam: SpamConfig { enabled: false, ..SpamConfig::default() },
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        },
    )
    .unwrap();
    for name in ["example.org", "client.example.org", "_dmarc.example.org", "example.com", "_dmarc.example.com"] {
        smtp.dns_cache().pin_no_txt(name);
    }
    let (shutdown, rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mx = listener.local_addr().unwrap();
    tokio::spawn(uwumail_smtp::serve(smtp.clone(), listener, ListenerKind::Mx, rx));
    Server { router: Jmap::new(smtp).router(), store, mx, _shutdown: shutdown, _dir: dir }
}

fn basic(login: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}")))
}

impl Server {
    async fn request(&self, request: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).map(|value| value.to_str().unwrap().to_owned());
        (status, to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec(), content_type)
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body = json!({ "using": USING, "methodCalls": calls });
        let request = Request::post("/jmap/api")
            .header(header::AUTHORIZATION, basic(login))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, bytes, _) = self.request(request).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }

    /// Uploads a script the way a client does; returns its blob id.
    async fn upload(&self, login: &str, script: &str) -> String {
        let account = self.account_id(login).await;
        let request = Request::post(format!("/jmap/upload/{account}/"))
            .header(header::AUTHORIZATION, basic(login))
            .header(header::CONTENT_TYPE, "application/sieve")
            .body(Body::from(script.to_owned()))
            .unwrap();
        let (status, bytes, _) = self.request(request).await;
        assert_eq!(status, StatusCode::CREATED);
        let uploaded: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(uploaded["type"], "application/sieve");
        uploaded["blobId"].as_str().unwrap().to_owned()
    }

    async fn download(&self, login: &str, account: &str, blob_id: &str) -> (StatusCode, String, Option<String>) {
        let request = Request::get(format!("/jmap/download/{account}/{blob_id}/rules.siv"))
            .header(header::AUTHORIZATION, basic(login))
            .body(Body::empty())
            .unwrap();
        let (status, bytes, content_type) = self.request(request).await;
        (status, String::from_utf8_lossy(&bytes).into_owned(), content_type)
    }

    /// Hands in a message over SMTP, the way another server would.
    async fn deliver(&self, to: &str, headers: &str) -> String {
        let mut reader = BufReader::new(TcpStream::connect(self.mx).await.unwrap());
        let mut command = async |line: &str| {
            if !line.is_empty() {
                reader.get_mut().write_all(format!("{line}\r\n").as_bytes()).await.unwrap();
            }
            let mut reply = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                reply.push_str(&line);
                if line.len() < 4 || line.as_bytes()[3] != b'-' {
                    return reply;
                }
            }
        };
        assert!(command("").await.starts_with("220"));
        assert!(command("EHLO client.example.org").await.starts_with("250"));
        assert!(command("MAIL FROM:<news@example.org>").await.starts_with("250"));
        assert!(command(&format!("RCPT TO:<{to}>")).await.starts_with("250"));
        assert!(command("DATA").await.starts_with("354"));
        command(&format!("{headers}\r\n\r\nMiau\r\n.")).await
    }

    /// The messages in a mailbox, found by its full name path.
    async fn folder(&self, login: &str, path: &[&str]) -> Vec<EmailSummary> {
        let account = self.store.account(login).await.unwrap().unwrap();
        let mailboxes = self.store.mailboxes(account.id).await.unwrap();
        let mut parent = None;
        let mut id = None;
        for name in path {
            let found = mailboxes.iter().find(|m| m.parent_id == parent && m.name == *name).expect("no such folder");
            parent = Some(found.id);
            id = Some(found.id);
        }
        self.store.emails_in_mailbox(id.unwrap(), 50).await.unwrap()
    }

    async fn wait_for(&self, login: &str, path: &[&str], count: usize) -> Vec<EmailSummary> {
        let started = Instant::now();
        loop {
            let emails = self.folder(login, path).await;
            if emails.len() >= count {
                return emails;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "{login} has {} of {count} in {path:?}", emails.len());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn mailbox_id(&self, login: &str, path: &[&str]) -> String {
        let account = self.store.account(login).await.unwrap().unwrap();
        let mailboxes = self.store.mailboxes(account.id).await.unwrap();
        let mut parent = None;
        for name in path {
            parent = Some(mailboxes.iter().find(|m| m.parent_id == parent && m.name == *name).unwrap().id);
        }
        format!("m{}", parent.unwrap())
    }
}

fn args<'a>(responses: &'a [Value], index: usize, name: &str) -> &'a Value {
    assert_eq!(responses[index][0], name, "response {index}: {}", responses[index]);
    &responses[index][1]
}

/// The rules the webmail and the apps write (SPEC section 3), for two folders.
fn generated_rules(work: &str, lists: &str) -> String {
    format!(
        r#"# Mail rules managed by UwUMail. Edit them in UwUMail; edits made elsewhere switch UwUMail to text mode.
# uwumail-rules: {{"v":1,"rules":[]}}
require ["fileinto", "imap4flags", "mailboxid", "copy"];

# Boss "the one"
if allof (header :contains "from" "boss@example.org", not header :is "subject" "ignore \\* me") {{
    addflag "\\Seen";
    redirect :copy "nyu@example.com";
    fileinto :mailboxid "{work}" "Work/Boss";
    stop;
}}

# Lists
if anyof (header :matches "list-id" "*cats.example.org*", address :is :all ["to", "cc"] "cats@example.org") {{
    addflag "\\Flagged";
    fileinto :mailboxid "{lists}" "Inbox/Lists";
}}
"#
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_names_the_extensions_delivery_carries_out() {
    let server = server().await;
    let request =
        Request::get("/jmap/session").header(header::AUTHORIZATION, basic("mini@example.com")).body(Body::empty());
    let (status, body, _) = server.request(request.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_slice(&body).unwrap();
    let account = server.account_id("mini@example.com").await;
    assert_eq!(session["capabilities"][SIEVE]["implementation"], "UwUMail Server");
    assert_eq!(session["primaryAccounts"][SIEVE], account);
    let limits = &session["accounts"][&account]["accountCapabilities"][SIEVE];
    assert_eq!(limits["maxSizeScriptName"], 512);
    assert_eq!(limits["maxSizeScript"], 65536);
    assert_eq!(limits["maxNumberScripts"], 16);
    assert_eq!(limits["maxNumberRedirects"], 1);
    assert_eq!(limits["notificationMethods"], Value::Null);
    assert_eq!(limits["externalLists"], Value::Null);
    let extensions: Vec<&str> =
        limits["sieveExtensions"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    for needed in ["fileinto", "imap4flags", "mailboxid", "copy", "mailbox", "envelope", "body", "variables"] {
        assert!(extensions.contains(&needed), "{needed} missing from {extensions:?}");
    }
    for missing in ["vacation", "reject", "enotify", "include", "regex"] {
        assert!(!extensions.contains(&missing), "{missing} is not carried out");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rules_from_the_apps_sort_mail_that_arrives() {
    let server = server().await;
    let mini = server.account_id("mini@example.com").await;
    // The folders the rules point at, made the way the webmail makes them.
    let responses = server
        .api(
            "mini@example.com",
            json!([
                ["Mailbox/set", { "accountId": mini, "create": { "w": { "name": "Work" } } }, "0"],
                ["Mailbox/set", { "accountId": mini, "create": { "b": { "name": "Boss", "parentId": "#w" } } }, "1"],
            ]),
        )
        .await;
    let work = args(&responses, 1, "Mailbox/set")["created"]["b"]["id"].as_str().unwrap().to_owned();
    let inbox = server.mailbox_id("mini@example.com", &["Inbox"]).await;
    let responses = server
        .api(
            "mini@example.com",
            json!([["Mailbox/set", { "accountId": mini, "create": { "l": { "name": "Lists", "parentId": inbox } } }, "0"]]),
        )
        .await;
    let lists = args(&responses, 0, "Mailbox/set")["created"]["l"]["id"].as_str().unwrap().to_owned();

    // Upload, create and activate in one go, as saveMailRules does.
    let script = generated_rules(&work, &lists);
    let blob = server.upload("mini@example.com", &script).await;
    let responses = server
        .api(
            "mini@example.com",
            json!([["SieveScript/set", {
                "accountId": mini,
                "create": { "rules": { "name": "UwUMail", "blobId": blob } },
                "onSuccessActivateScript": "#rules"
            }, "0"]]),
        )
        .await;
    let created = &args(&responses, 0, "SieveScript/set")["created"]["rules"];
    assert_eq!(created["isActive"], true, "{created}");
    assert_eq!(created["name"], "UwUMail");
    let script_id = created["id"].as_str().unwrap().to_owned();

    // The script is downloadable under its blob id, as a script.
    let (status, content, content_type) =
        server.download("mini@example.com", &mini, created["blobId"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content, script);
    assert_eq!(content_type.as_deref(), Some("application/sieve"));

    // The boss: read, filed by mailbox id, and a copy for Nyu on this server.
    let reply = server.deliver("mini@example.com", "From: Boss <boss@example.org>\r\nSubject: Quarterly numbers").await;
    assert!(reply.starts_with("250"), "{reply}");
    let filed = server.wait_for("mini@example.com", &["Work", "Boss"], 1).await;
    assert_eq!(filed[0].subject, "Quarterly numbers");
    assert_eq!(filed[0].keywords, ["$seen"]);
    assert_eq!(server.wait_for("nyu@example.com", &["Inbox"], 1).await[0].subject, "Quarterly numbers");

    // A list: flagged, into a folder under the inbox, no redirect.
    let reply = server
        .deliver("mini@example.com", "From: news@example.org\r\nList-Id: Cats <cats.example.org>\r\nSubject: Purr")
        .await;
    assert!(reply.starts_with("250"), "{reply}");
    let filed = server.wait_for("mini@example.com", &["Inbox", "Lists"], 1).await;
    assert_eq!(filed[0].keywords, ["$flagged"]);
    assert!(server.folder("mini@example.com", &["Inbox"]).await.is_empty());
    assert_eq!(server.folder("nyu@example.com", &["Inbox"]).await.len(), 1);

    // Anything else stays in the inbox.
    assert!(server.deliver("mini@example.com", "From: news@example.org\r\nSubject: Hallo").await.starts_with("250"));
    assert_eq!(server.wait_for("mini@example.com", &["Inbox"], 1).await[0].subject, "Hallo");

    // Nyu has no rules of her own: Mini's never touch her mail.
    assert!(
        server.deliver("nyu@example.com", "From: Boss <boss@example.org>\r\nSubject: For Nyu").await.starts_with("250")
    );
    let inbox = server.wait_for("nyu@example.com", &["Inbox"], 2).await;
    assert!(inbox.iter().all(|email| email.keywords.is_empty()));

    // Switched off, the rules stop sorting.
    let responses = server
        .api(
            "mini@example.com",
            json!([["SieveScript/set", { "accountId": mini, "onSuccessDeactivateScript": true }, "0"]]),
        )
        .await;
    assert_eq!(args(&responses, 0, "SieveScript/set")["updated"][&script_id]["isActive"], false);
    assert!(
        server.deliver("mini@example.com", "From: Boss <boss@example.org>\r\nSubject: Again").await.starts_with("250")
    );
    assert_eq!(server.wait_for("mini@example.com", &["Inbox"], 2).await.len(), 2);
    assert_eq!(server.folder("mini@example.com", &["Work", "Boss"]).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_are_checked_named_and_kept_apart() {
    let server = server().await;
    let mini = server.account_id("mini@example.com").await;
    let nyu = server.account_id("nyu@example.com").await;

    // Invalid Sieve and extensions that are not carried out are refused, with the line.
    let broken = server.upload("mini@example.com", "#comment\nInvalidSieveCommand\n").await;
    let vacation = server.upload("mini@example.com", "require \"vacation\";\nvacation \"away\";\n").await;
    let fine = server.upload("mini@example.com", "require \"fileinto\";\nfileinto \"Archive\";\n").await;
    let responses = server
        .api(
            "mini@example.com",
            json!([
                ["SieveScript/validate", { "accountId": mini, "blobId": broken }, "0"],
                ["SieveScript/validate", { "accountId": mini, "blobId": fine }, "1"],
                ["SieveScript/set", { "accountId": mini, "create": {
                    "a": { "name": "broken", "blobId": broken },
                    "b": { "name": "vacation", "blobId": vacation },
                    "c": { "name": "fine", "blobId": fine },
                    "d": { "name": "missing", "blobId": "bnope" },
                    "e": { "name": "active", "blobId": fine, "isActive": true }
                } }, "2"],
            ]),
        )
        .await;
    let error = &args(&responses, 0, "SieveScript/validate")["error"];
    assert_eq!(error["type"], "invalidSieve");
    assert!(error["description"].as_str().unwrap().contains("line 2"), "{error}");
    assert_eq!(args(&responses, 1, "SieveScript/validate")["error"], Value::Null);
    let set = args(&responses, 2, "SieveScript/set");
    assert_eq!(set["notCreated"]["a"]["type"], "invalidSieve");
    assert_eq!(set["notCreated"]["b"]["type"], "invalidSieve");
    assert_eq!(set["notCreated"]["d"]["type"], "blobNotFound");
    assert_eq!(set["notCreated"]["e"]["type"], "invalidProperties");
    let fine_id = set["created"]["c"]["id"].as_str().unwrap().to_owned();
    assert_eq!(set["created"]["c"]["isActive"], false);

    // Names are unique, and a script without a name gets one.
    let responses = server
        .api(
            "mini@example.com",
            json!([["SieveScript/set", { "accountId": mini, "create": {
                "same": { "name": "fine", "blobId": fine },
                "unnamed": { "name": null, "blobId": fine }
            } }, "0"]]),
        )
        .await;
    let set = args(&responses, 0, "SieveScript/set");
    assert_eq!(set["notCreated"]["same"]["type"], "alreadyExists");
    assert_eq!(set["notCreated"]["same"]["existingId"], fine_id.as_str());
    assert_eq!(set["created"]["unnamed"]["name"], "script-1");
    let unnamed = set["created"]["unnamed"]["id"].as_str().unwrap().to_owned();
    let state = set["newState"].as_str().unwrap().to_owned();

    // Activation moves from one to the other; the active one cannot be destroyed.
    let responses = server
        .api(
            "mini@example.com",
            json!([
                ["SieveScript/set", { "accountId": mini, "onSuccessActivateScript": fine_id }, "0"],
                ["SieveScript/set", { "accountId": mini, "onSuccessActivateScript": unnamed }, "1"],
                ["SieveScript/set", { "accountId": mini, "destroy": [unnamed] }, "2"],
                ["SieveScript/changes", { "accountId": mini, "sinceState": state }, "3"],
                ["SieveScript/query", { "accountId": mini, "filter": { "isActive": true } }, "4"],
                ["SieveScript/query", { "accountId": mini, "sort": [{ "property": "name" }] }, "5"],
                ["SieveScript/get", { "accountId": mini, "ids": [unnamed], "properties": ["isActive"] }, "6"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 0, "SieveScript/set")["updated"][&fine_id]["isActive"], true);
    let switched = args(&responses, 1, "SieveScript/set");
    assert_eq!(switched["updated"][&fine_id]["isActive"], false);
    assert_eq!(switched["updated"][&unnamed]["isActive"], true);
    assert_eq!(args(&responses, 2, "SieveScript/set")["notDestroyed"][&unnamed]["type"], "sieveIsActive");
    let changes = args(&responses, 3, "SieveScript/changes");
    assert_eq!(changes["updated"].as_array().unwrap().len(), 2, "{changes}");
    assert_eq!(args(&responses, 4, "SieveScript/query")["ids"], json!([unnamed]));
    assert_eq!(args(&responses, 5, "SieveScript/query")["ids"], json!([fine_id, unnamed]));
    assert_eq!(args(&responses, 6, "SieveScript/get")["list"][0], json!({ "id": unnamed, "isActive": true }));

    // A new blob replaces the content, and the blob id follows it.
    let new_content = server.upload("mini@example.com", "keep;\n").await;
    let responses = server
        .api(
            "mini@example.com",
            json!([["SieveScript/set", { "accountId": mini, "update": {
                fine_id.clone(): { "blobId": new_content, "name": "renamed" }
            } }, "0"]]),
        )
        .await;
    let updated = &args(&responses, 0, "SieveScript/set")["updated"][&fine_id];
    assert_eq!(updated["blobId"], new_content.as_str());

    // Nyu sees none of it: not the scripts, not their blobs, not Mini's uploads.
    let responses = server
        .api(
            "nyu@example.com",
            json!([
                ["SieveScript/get", { "accountId": nyu, "ids": [fine_id, unnamed] }, "0"],
                ["SieveScript/set", { "accountId": nyu, "onSuccessActivateScript": fine_id, "destroy": [unnamed] }, "1"],
                ["SieveScript/set", { "accountId": nyu, "create": { "x": { "name": "copy", "blobId": fine } } }, "2"],
                ["SieveScript/get", { "accountId": mini }, "3"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 0, "SieveScript/get")["notFound"], json!([fine_id, unnamed]));
    let set = args(&responses, 1, "SieveScript/set");
    assert_eq!(set["notDestroyed"][&unnamed]["type"], "notFound");
    assert_eq!(set["updated"], Value::Null, "Mini's script is not Nyu's to activate");
    assert_eq!(args(&responses, 2, "SieveScript/set")["notCreated"]["x"]["type"], "blobNotFound");
    assert_eq!(responses[3][1]["type"], "accountNotFound");
    let (status, _, _) = server.download("nyu@example.com", &nyu, &new_content).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let mini_account = server.store.account("mini@example.com").await.unwrap().unwrap();
    assert!(server.store.active_sieve_script(mini_account.id).await.unwrap().is_some(), "Mini's rules still run");

    // Deactivated, the script can go.
    let responses = server
        .api(
            "mini@example.com",
            json!([
                ["SieveScript/set", { "accountId": mini, "onSuccessDeactivateScript": true }, "0"],
                ["SieveScript/set", { "accountId": mini, "destroy": [unnamed] }, "1"],
            ]),
        )
        .await;
    assert_eq!(args(&responses, 1, "SieveScript/set")["destroyed"], json!([unnamed]));
}

/// security-audit-0.7.0 S-46: a blob named as a script's content was read whole before its size
/// was looked at -- an upload or a message of up to 50 MB, as often as one call names it. The size
/// is checked first now; the content of a blob that is too large is never read.
#[tokio::test(flavor = "multi_thread")]
async fn a_blob_too_large_for_a_script_is_not_read() {
    let server = server().await;
    let account = server.account_id("mini@example.com").await;
    let big = format!("keep;\n{}", "#".repeat(2 * 1024 * 1024));
    let blob = server.upload("mini@example.com", &big).await;
    // Take the content away: only its recorded size is left to answer with.
    let hash = blob.trim_start_matches('b');
    let path = server._dir.path().join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(hash);
    std::fs::remove_file(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let creates: serde_json::Map<String, Value> =
        (0..50).map(|n| (format!("s{n}"), json!({ "name": format!("big {n}"), "blobId": blob }))).collect();
    let responses = server
        .api(
            "mini@example.com",
            json!([
                ["SieveScript/set", { "accountId": account, "create": creates }, "0"],
                ["SieveScript/validate", { "accountId": account, "blobId": blob }, "1"]
            ]),
        )
        .await;
    let not_created = responses[0][1]["notCreated"].as_object().unwrap();
    assert_eq!(not_created.len(), 50);
    assert!(not_created.values().all(|error| error["type"] == "tooLarge"), "{not_created:?}");
    assert_eq!(responses[1][1]["error"]["type"], "tooLarge", "{}", responses[1][1]);
}
