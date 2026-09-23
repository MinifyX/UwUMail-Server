//! JMAP Contacts end to end, next to CardDAV on the same store, the way the webmail, the UwUMail
//! apps and a phone would use them together.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::{ClientInfo, Jmap};
use uwumail_smtp::{DeliveryConfig, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
use uwumail_store::{NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const MINI: &str = "mini@example.de";
const NYU: &str = "nyu@example.de";
const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:contacts"];

struct Server {
    router: Router,
    store: Store,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    for user in ["mini", "nyu"] {
        store
            .create_account(NewAccount {
                address: format!("{user}@example.de"),
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
            hostname: "mail.example.de".into(),
            smtp: SmtpConfig::default(),
            spam: Default::default(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        },
    )
    .unwrap();
    let dav =
        Dav::new(store.clone(), DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() });
    Server { router: Jmap::new(smtp).router().merge(dav.router()), store, _dir: dir }
}

fn basic(login: &str) -> String {
    format!("Basic {}", BASE64.encode(format!("{login}:{PASSWORD}")))
}

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    body: String,
}

impl Server {
    async fn send(&self, login: &str, method: &str, uri: &str, headers: &[(&str, &str)], body: String) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, basic(login))
            .header(header::HOST, "mail.example.de");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let etag = response.headers().get(header::ETAG).map(|v| v.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap();
        Reply { status, etag, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body = json!({ "using": USING, "methodCalls": calls }).to_string();
        let reply = self.send(login, "POST", "/jmap/api", &[("content-type", "application/json")], body).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        let response: Value = serde_json::from_str(&reply.body).unwrap();
        response["methodResponses"].as_array().unwrap().clone()
    }

    /// One call, its response arguments.
    async fn call(&self, login: &str, method: &str, arguments: Value) -> Value {
        let responses = self.api(login, json!([[method, arguments, "0"]])).await;
        assert_eq!(responses[0][0], method, "{}", responses[0]);
        responses[0][1].clone()
    }

    async fn account_id(&self, login: &str) -> String {
        format!("a{}", self.store.account(login).await.unwrap().unwrap().id)
    }

    async fn state(&self, login: &str) -> String {
        let account = self.account_id(login).await;
        self.call(login, "AddressBook/get", json!({ "accountId": account, "ids": [] })).await["state"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The default address book's id, made on the way.
    async fn default_book(&self, login: &str) -> String {
        let account = self.account_id(login).await;
        let books = self.call(login, "AddressBook/get", json!({ "accountId": account })).await;
        books["list"].as_array().unwrap().iter().find(|b| b["isDefault"] == true).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn create_card(&self, login: &str, card: Value) -> String {
        let account = self.account_id(login).await;
        let set = self.call(login, "ContactCard/set", json!({ "accountId": account, "create": { "c": card } })).await;
        set["created"]["c"]["id"].as_str().unwrap_or_else(|| panic!("{set}")).to_owned()
    }

    async fn get_card(&self, login: &str, id: &str) -> Value {
        let account = self.account_id(login).await;
        let got = self.call(login, "ContactCard/get", json!({ "accountId": account, "ids": [id] })).await;
        got["list"][0].clone()
    }
}

fn person(given: &str, surname: &str, email: &str) -> Value {
    json!({
        "name": { "components": [{ "kind": "given", "value": given }, { "kind": "surname", "value": surname }] },
        "emails": { "e1": { "address": email } }
    })
}

const PHONE_CARD: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//iPhone OS 17.0//EN\r\nN:Katze;Nyu;;;\r\n\
FN:Nyu Katze\r\nEMAIL;type=INTERNET;type=HOME;type=pref:nyu@example.org\r\nTEL;type=CELL:+49 170 1234567\r\n\
X-ABSHOWAS:PERSON\r\nUID:phone-1\r\nEND:VCARD\r\n";

const SYNC: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:sync-collection xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:sync-token>TOKEN</d:sync-token>
<d:sync-level>1</d:sync-level><d:prop><d:getetag/><card:address-data/></d:prop></d:sync-collection>"#;

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap_or_else(|| panic!("{start} not in {text}")) + start.len();
    let to = text[from..].find(end).unwrap_or_else(|| panic!("{end} not after {start} in {text}")) + from;
    &text[from..to]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_offers_contacts() {
    let server = server().await;
    let reply = server.send(MINI, "GET", "/jmap/session", &[], String::new()).await;
    let session: Value = serde_json::from_str(&reply.body).unwrap();
    let account = server.account_id(MINI).await;
    assert_eq!(session["capabilities"]["urn:ietf:params:jmap:contacts"], json!({}));
    assert_eq!(session["primaryAccounts"]["urn:ietf:params:jmap:contacts"], account);
    assert_eq!(
        session["accounts"][&account]["accountCapabilities"]["urn:ietf:params:jmap:contacts"],
        json!({ "maxAddressBooksPerCard": 1, "mayCreateAddressBook": true })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn address_books_are_made_changed_and_removed() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let books = server.call(MINI, "AddressBook/get", json!({ "accountId": account })).await;
    let list = books["list"].as_array().unwrap();
    assert_eq!(list.len(), 1, "the first address book appears by itself: {books}");
    assert_eq!(list[0]["name"], "Kontakte", "named in the server's language");
    assert_eq!(list[0]["isDefault"], true);
    assert_eq!(list[0]["myRights"]["mayDelete"], false, "the only one stays");
    let personal = list[0]["id"].as_str().unwrap().to_owned();
    let before = books["state"].as_str().unwrap().to_owned();

    let set = server
        .call(
            MINI,
            "AddressBook/set",
            json!({
                "accountId": account,
                "create": {
                    "f": { "name": "Familie", "description": "die Lieben", "sortOrder": 2 },
                    "bad": { "name": "", "color": "#ff0000" }
                },
                "onSuccessSetIsDefault": "#f"
            }),
        )
        .await;
    assert_eq!(set["notCreated"]["bad"]["type"], "invalidProperties", "{set}");
    assert!(set["updated"].is_null(), "not the default when something failed: {set}");
    let family = set["created"]["f"]["id"].as_str().unwrap().to_owned();
    assert_eq!(set["created"]["f"]["isDefault"], false);

    let set = server
        .call(
            MINI,
            "AddressBook/set",
            json!({ "accountId": account, "update": { &family: { "name": "Familie & Freunde" } }, "onSuccessSetIsDefault": &family }),
        )
        .await;
    assert_eq!(set["updated"][&family]["isDefault"], true, "{set}");
    assert_eq!(set["updated"][&personal]["isDefault"], false, "{set}");
    let changes = server.call(MINI, "AddressBook/changes", json!({ "accountId": account, "sinceState": before })).await;
    assert_eq!(changes["created"], json!([&family]));
    assert_eq!(changes["updated"], json!([&personal]));

    // A card makes the address book one with contents.
    server.create_card(MINI, json!({ "addressBookIds": { &family: true }, "name": { "full": "Oma" } })).await;
    let refused = server.call(MINI, "AddressBook/set", json!({ "accountId": account, "destroy": [&family] })).await;
    assert_eq!(refused["notDestroyed"][&family]["type"], "addressBookHasContents", "{refused}");
    let gone = server
        .call(
            MINI,
            "AddressBook/set",
            json!({ "accountId": account, "destroy": [&family], "onDestroyRemoveContents": true }),
        )
        .await;
    assert_eq!(gone["destroyed"], json!([&family]), "{gone}");
    assert_eq!(server.default_book(MINI).await, personal, "the default moves back");
    let last = server
        .call(
            MINI,
            "AddressBook/set",
            json!({ "accountId": account, "destroy": [&personal], "onDestroyRemoveContents": true }),
        )
        .await;
    assert_eq!(last["notDestroyed"][&personal]["type"], "forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_are_made_changed_and_deleted() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let book = server.default_book(MINI).await;
    let before = server.state(MINI).await;

    // Without an address book, a card goes into the default one; the server fills in the rest.
    let set = server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "create": { "n": person("Nyu", "Katze", "nyu@example.org") } }),
        )
        .await;
    let created = &set["created"]["n"];
    let id = created["id"].as_str().unwrap_or_else(|| panic!("{set}")).to_owned();
    assert_eq!(created["addressBookIds"], json!({ &book: true }));
    assert_eq!(created["@type"], "Card");
    assert!(created["uid"].as_str().unwrap().starts_with("urn:uuid:"), "{created}");
    assert!(created["updated"].is_string() && created["created"].is_string(), "{created}");

    let card = server.get_card(MINI, &id).await;
    assert_eq!(card["name"]["components"][0]["value"], "Nyu");
    assert_eq!(card["name"]["full"], "Nyu Katze", "a full name is derived for CardDAV: {card}");
    assert_eq!(card["emails"]["e1"]["address"], "nyu@example.org");
    assert!(card.get("vCard").is_none(), "conversion hints only when asked for: {card}");

    let patch = json!({
        "emails/e2": { "address": "nyu@work.example", "contexts": { "work": true } },
        "phones": { "p1": { "number": "+49 30 1234", "features": { "mobile": true } } },
        "anniversaries": { "b": { "kind": "birth", "date": { "@type": "PartialDate", "year": 1990, "month": 5, "day": 17 } } },
        "notes": { "n": { "note": "mag Thunfisch" } }
    });
    let updated = server.call(MINI, "ContactCard/set", json!({ "accountId": account, "update": { &id: patch } })).await;
    assert!(updated["updated"][&id]["updated"].is_string(), "{updated}");
    let card = server.get_card(MINI, &id).await;
    assert_eq!(card["emails"]["e2"]["address"], "nyu@work.example", "{card}");
    assert_eq!(card["phones"]["p1"]["number"], "+49 30 1234");
    assert_eq!(card["anniversaries"]["b"]["date"]["day"], 17);
    assert_eq!(card["notes"]["n"]["note"], "mag Thunfisch");

    let refused = server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "update": {
                &id: { "uid": "anders" },
                "k999999": { "notes": null }
            }, "create": {
                "noemail": { "emails": { "e": { "label": "ohne Adresse" } } },
                "twice": { "uid": card["uid"].clone() },
                "html": { "media": { "m": { "kind": "photo", "uri": "data:text/html;base64,PGI+" } } },
                "nobook": { "addressBookIds": { "b999999": true } }
            } }),
        )
        .await;
    assert_eq!(refused["notUpdated"][&id]["properties"], json!(["uid"]), "{refused}");
    assert_eq!(refused["notUpdated"]["k999999"]["type"], "notFound");
    assert_eq!(refused["notCreated"]["noemail"]["properties"], json!(["emails"]));
    assert_eq!(refused["notCreated"]["twice"]["type"], "alreadyExists");
    assert_eq!(refused["notCreated"]["html"]["properties"], json!(["media"]));
    assert_eq!(refused["notCreated"]["nobook"]["properties"], json!(["addressBookIds"]));

    let changes =
        server.call(MINI, "ContactCard/changes", json!({ "accountId": account, "sinceState": &before })).await;
    assert_eq!(changes["created"], json!([&id]));
    let middle = server.state(MINI).await;
    let destroyed =
        server.call(MINI, "ContactCard/set", json!({ "accountId": account, "destroy": [&id, "k999999"] })).await;
    assert_eq!(destroyed["destroyed"], json!([&id]));
    assert_eq!(destroyed["notDestroyed"]["k999999"]["type"], "notFound");
    let changes = server.call(MINI, "ContactCard/changes", json!({ "accountId": account, "sinceState": middle })).await;
    assert_eq!(changes["destroyed"], json!([&id]));
    let gone = server.call(MINI, "ContactCard/get", json!({ "accountId": account, "ids": [&id] })).await;
    assert_eq!(gone["notFound"], json!([&id]));
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_move_between_address_books() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let personal = server.default_book(MINI).await;
    let set = server
        .call(MINI, "AddressBook/set", json!({ "accountId": account, "create": { "w": { "name": "Arbeit" } } }))
        .await;
    let work = set["created"]["w"]["id"].as_str().unwrap().to_owned();
    let id = server.create_card(MINI, person("Leni", "Muster", "leni@example.org")).await;
    let moved = server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "update": { &id: { format!("addressBookIds/{personal}"): null, format!("addressBookIds/{work}"): true } } }),
        )
        .await;
    assert!(moved["updated"][&id].is_object(), "{moved}");
    assert_eq!(server.get_card(MINI, &id).await["addressBookIds"], json!({ &work: true }));
    let both = server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "update": { &id: { "addressBookIds": { &work: true, &personal: true } } } }),
        )
        .await;
    assert_eq!(both["notUpdated"][&id]["properties"], json!(["addressBookIds"]), "one book per card: {both}");
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_filter_sort_and_page() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let book = server.default_book(MINI).await;
    let nyu = server.create_card(MINI, person("Nyu", "Katze", "nyu@example.org")).await;
    let leni = server.create_card(MINI, person("Leni", "Muster", "leni@example.org")).await;
    let mut firm = person("Anna", "Berg", "anna@firma.example");
    firm["organizations"] = json!({ "o": { "name": "Katzenfutter AG" } });
    firm["phones"] = json!({ "p": { "number": "+49 89 555" } });
    let anna = server.create_card(MINI, firm).await;
    let query = |filter: Value, sort: Value| json!({ "accountId": account, "filter": filter, "sort": sort });

    let found = server.call(MINI, "ContactCard/query", query(json!({ "text": "katze" }), Value::Null)).await;
    assert_eq!(found["ids"], json!([&nyu, &anna]), "names and organizations: {found}");
    let found = server.call(MINI, "ContactCard/query", query(json!({ "email": "LENI@" }), Value::Null)).await;
    assert_eq!(found["ids"], json!([&leni]));
    let found = server.call(MINI, "ContactCard/query", query(json!({ "name/surname": "muster" }), Value::Null)).await;
    assert_eq!(found["ids"], json!([&leni]));
    let found = server.call(MINI, "ContactCard/query", query(json!({ "phone": "555" }), Value::Null)).await;
    assert_eq!(found["ids"], json!([&anna]));
    let found = server
        .call(
            MINI,
            "ContactCard/query",
            query(
                json!({ "operator": "NOT", "conditions": [{ "organization": "futter" }] }),
                json!([{ "property": "name/surname" }]),
            ),
        )
        .await;
    assert_eq!(found["ids"], json!([&nyu, &leni]));
    let found = server
        .call(
            MINI,
            "ContactCard/query",
            query(json!({ "inAddressBook": &book }), json!([{ "property": "name/given", "isAscending": false }])),
        )
        .await;
    assert_eq!(found["ids"], json!([&nyu, &leni, &anna]));
    let future = server
        .call(MINI, "ContactCard/query", query(json!({ "createdAfter": "2200-01-01T00:00:00Z" }), Value::Null))
        .await;
    assert_eq!(future["ids"], json!([]));

    let page = server
        .call(
            MINI,
            "ContactCard/query",
            json!({ "accountId": account, "position": 1, "limit": 1, "calculateTotal": true }),
        )
        .await;
    assert_eq!(
        (page["ids"].clone(), page["total"].clone(), page["position"].clone()),
        (json!([&leni]), json!(3), json!(1))
    );

    let bad = server
        .api(MINI, json!([["ContactCard/query", { "accountId": account, "filter": { "colour": "red" } }, "0"]]))
        .await;
    assert_eq!(bad[0][1]["type"], "unsupportedFilter");
    let bad = server
        .api(MINI, json!([["ContactCard/query", { "accountId": account, "sort": [{ "property": "email" }] }, "0"]]))
        .await;
    assert_eq!(bad[0][1]["type"], "unsupportedSort");
}

#[tokio::test(flavor = "multi_thread")]
async fn carddav_and_jmap_see_each_others_changes() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let book = server.default_book(MINI).await;
    let collection = "/dav/addressbooks/mini@example.de/contacts/";
    let before = server.state(MINI).await;

    // A phone stores a card over CardDAV: JMAP clients hear about it and can read it.
    let router = server.router.clone();
    let push = tokio::spawn(async move {
        let mut request = Request::get("/jmap/eventsource/?types=ContactCard&closeafter=state&ping=0")
            .header(header::AUTHORIZATION, basic(MINI))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = router.oneshot(request).await.unwrap();
        String::from_utf8(to_bytes(response.into_body(), 1024 * 1024).await.unwrap().to_vec()).unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let put = server
        .send(MINI, "PUT", &format!("{collection}phone-1.vcf"), &[("content-type", "text/vcard")], PHONE_CARD.into())
        .await;
    assert_eq!(put.status, StatusCode::CREATED, "{}", put.body);
    let pushed = tokio::time::timeout(std::time::Duration::from_secs(10), push).await.unwrap().unwrap();
    assert!(pushed.contains("\"ContactCard\""), "{pushed}");

    let changes = server.call(MINI, "ContactCard/changes", json!({ "accountId": account, "sinceState": before })).await;
    let phone_id = changes["created"][0].as_str().unwrap_or_else(|| panic!("{changes}")).to_owned();
    let card = server.get_card(MINI, &phone_id).await;
    assert_eq!(card["name"]["full"], "Nyu Katze");
    assert_eq!(card["uid"], "phone-1");
    assert_eq!(card["addressBookIds"], json!({ &book: true }));

    // The webmail changes it: the phone gets it back with what JMAP does not know about.
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", "")).await;
    let token = between(&sync.body, "<d:sync-token>", "</d:sync-token>").to_owned();
    server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "update": { &phone_id: { "notes": { "n": { "note": "neu" } } } } }),
        )
        .await;
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", &token)).await;
    assert_eq!(sync.status, StatusCode::MULTI_STATUS, "{}", sync.body);
    assert!(sync.body.contains("NOTE"), "{}", sync.body);
    assert!(sync.body.contains("X-ABSHOWAS:PERSON"), "the phone's own data stays: {}", sync.body);
    assert!(sync.body.contains("VERSION:3.0"), "{}", sync.body);
    let fetched = server.send(MINI, "GET", &format!("{collection}phone-1.vcf"), &[], String::new()).await;
    let etag = fetched.etag.unwrap();
    // The phone can store it back unchanged: CardDAV accepts what JMAP wrote.
    let again = server
        .send(
            MINI,
            "PUT",
            &format!("{collection}phone-1.vcf"),
            &[("content-type", "text/vcard"), ("if-match", &etag)],
            fetched.body.clone(),
        )
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.body);

    // A card made over JMAP is a vCard 3.0 a phone can read.
    let web_id = server.create_card(MINI, person("Leni", "Muster", "leni@example.org")).await;
    let sync = server.send(MINI, "REPORT", collection, &[("depth", "1")], SYNC.replace("TOKEN", &token)).await;
    assert!(sync.body.contains("leni@example.org"), "{}", sync.body);

    // The phone deletes, a new address book appears: JMAP sees both.
    let middle = server.state(MINI).await;
    let deleted = server.send(MINI, "DELETE", &format!("{collection}phone-1.vcf"), &[], String::new()).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let mkcol = r#"<?xml version="1.0" encoding="utf-8"?>
<d:mkcol xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:set><d:prop><d:resourcetype><d:collection/><card:addressbook/></d:resourcetype>
<d:displayname>Verein</d:displayname></d:prop></d:set></d:mkcol>"#;
    let made = server.send(MINI, "MKCOL", "/dav/addressbooks/mini@example.de/club/", &[], mkcol.into()).await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let changes =
        server.call(MINI, "ContactCard/changes", json!({ "accountId": account, "sinceState": &middle })).await;
    assert_eq!(changes["destroyed"], json!([&phone_id]));
    let books = server.call(MINI, "AddressBook/changes", json!({ "accountId": account, "sinceState": &middle })).await;
    assert_eq!(books["created"].as_array().unwrap().len(), 1, "{books}");
    assert_eq!(server.get_card(MINI, &web_id).await["name"]["full"], "Leni Muster");
}

#[tokio::test(flavor = "multi_thread")]
async fn nobody_reaches_into_another_account() {
    let server = server().await;
    let mini = server.account_id(MINI).await;
    let nyu = server.account_id(NYU).await;
    let id = server.create_card(MINI, person("Geheim", "Kontakt", "geheim@example.org")).await;
    let mini_book = server.default_book(MINI).await;

    let foreign = server.api(NYU, json!([["ContactCard/get", { "accountId": &mini, "ids": [&id] }, "0"]])).await;
    assert_eq!(foreign[0][1]["type"], "accountNotFound");
    let got = server.call(NYU, "ContactCard/get", json!({ "accountId": &nyu, "ids": [&id] })).await;
    assert_eq!(got["notFound"], json!([&id]));
    let all = server.call(NYU, "ContactCard/get", json!({ "accountId": &nyu, "ids": null })).await;
    assert_eq!(all["list"], json!([]));
    let found = server.call(NYU, "ContactCard/query", json!({ "accountId": &nyu })).await;
    assert_eq!(found["ids"], json!([]));
    let set = server
        .call(
            NYU,
            "ContactCard/set",
            json!({ "accountId": &nyu, "update": { &id: { "notes": null } }, "destroy": [&id],
                    "create": { "x": { "addressBookIds": { &mini_book: true } } } }),
        )
        .await;
    assert_eq!(set["notUpdated"][&id]["type"], "notFound", "{set}");
    assert_eq!(set["notDestroyed"][&id]["type"], "notFound");
    assert_eq!(set["notCreated"]["x"]["properties"], json!(["addressBookIds"]));
    let books = server
        .call(
            NYU,
            "AddressBook/set",
            json!({ "accountId": &nyu, "update": { &mini_book: { "name": "meins" } }, "destroy": [&mini_book] }),
        )
        .await;
    assert_eq!(books["notUpdated"][&mini_book]["type"], "notFound");
    assert_eq!(books["notDestroyed"][&mini_book]["type"], "notFound");
    assert_eq!(server.get_card(MINI, &id).await["name"]["full"], "Geheim Kontakt");

    // Accounts without CardDAV have no contact methods either.
    server
        .store
        .update_account(
            MINI,
            uwumail_store::AccountUpdate {
                protocols: Some(uwumail_store::Protocols { carddav: false, ..Default::default() }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let off = server.api(MINI, json!([["ContactCard/query", { "accountId": &mini }, "0"]])).await;
    assert_eq!(off[0][1]["type"], "accountNotSupportedByMethod");
    let reply = server.send(MINI, "GET", "/jmap/session", &[], String::new()).await;
    assert!(!reply.body.contains("urn:ietf:params:jmap:contacts"), "{}", reply.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_stay_within_limits() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let big = format!("data:image/jpeg;base64,{}", "A".repeat(1024 * 1024));
    let set = server
        .call(
            MINI,
            "ContactCard/set",
            json!({ "accountId": account, "create": { "big": { "media": { "m": { "kind": "photo", "uri": big } } } } }),
        )
        .await;
    assert_eq!(set["notCreated"]["big"]["type"], "tooLarge", "{set}");
    let too_many: serde_json::Map<String, Value> = (0..501).map(|n| (format!("c{n}"), json!({}))).collect();
    let refused =
        server.api(MINI, json!([["ContactCard/set", { "accountId": account, "create": too_many }, "0"]])).await;
    assert_eq!(refused[0][1]["type"], "requestTooLarge");
}
