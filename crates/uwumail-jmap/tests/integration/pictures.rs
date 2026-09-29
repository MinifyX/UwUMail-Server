//! Sender pictures per address (`pictureUrl`, docs/jmap-remote.md) and the Libravatar provider
//! (`/avatar/`, docs/profile-pictures.md), with a stand-in for the internet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::{ClientInfo, Jmap};
use uwumail_smtp::avatars::{AvatarNet, BoxFuture, SrvRecords, libravatar_hash, libravatar_hashes};
use uwumail_smtp::egress::EgressError;
use uwumail_smtp::profile_pictures::{prepare, sample};
use uwumail_store::{
    DomainMaskedPolicy, MaskedMode, MaskedState, NewMaskedAddress, NewPicture, PictureOwner, PictureVisibility,
    ProfileUpdate, ShareRights,
};

use crate::common::{PASSWORD, Server, basic, server, smtp};

const FRIEND: &str = "friend@elsewhere.example";

/// DNS and web servers of other people, as the test wants them; remembers what was asked.
#[derive(Default)]
struct FakeNet {
    srv: HashMap<String, SrvRecords>,
    pages: HashMap<String, Vec<u8>>,
    asked: Mutex<Vec<String>>,
}

impl AvatarNet for FakeNet {
    fn get<'a>(&'a self, url: &'a str, _max: usize) -> BoxFuture<'a, Result<(String, Vec<u8>), EgressError>> {
        self.asked.lock().unwrap().push(url.to_owned());
        let page = self.pages.get(url).cloned();
        Box::pin(async move { page.map(|body| ("image/png".to_owned(), body)).ok_or(EgressError::Status(404)) })
    }

    fn srv<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<SrvRecords, ()>> {
        self.asked.lock().unwrap().push(name.to_owned());
        let records = self.srv.get(name).cloned().unwrap_or_default();
        Box::pin(async move { Ok(records) })
    }
}

struct Pictures {
    server: Server,
    router: Router,
    net: Arc<FakeNet>,
}

async fn pictures(net: FakeNet) -> Pictures {
    let server = server().await;
    let net = Arc::new(net);
    let jmap = Jmap::new(smtp(&server.store)).with_avatar_net(net.clone());
    let dav = Dav::new(
        server.store.clone(),
        DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() },
    );
    Pictures { router: jmap.router().merge(dav.router()), server, net }
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Pictures {
    async fn send(
        &self,
        login: Option<&str>,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut request = Request::builder().method(method).uri(uri).header(header::HOST, "mail.example.org");
        if let Some(login) = login {
            request = request.header(header::AUTHORIZATION, basic(login, PASSWORD));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ClientInfo { ip: "192.0.2.10".parse().unwrap(), https: true });
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 64 * 1024 * 1024).await.unwrap().to_vec();
        Answer { status, headers, body }
    }

    /// The picture `login` gets for `email`, with extra query parameters.
    async fn picture(&self, login: &str, email: &str, extra: &str) -> Answer {
        let account = self.server.account_id(login).await;
        let email = email.replace('@', "%40").replace('+', "%2B");
        self.send(Some(login), "GET", &format!("/jmap/picture/{account}?email={email}{extra}"), &[], Vec::new()).await
    }

    async fn api(&self, login: &str, calls: Value) -> Vec<Value> {
        let body =
            json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:contacts"], "methodCalls": calls });
        let answer = self
            .send(
                Some(login),
                "POST",
                "/jmap/api",
                &[("content-type", "application/json")],
                body.to_string().into_bytes(),
            )
            .await;
        assert_eq!(answer.status, StatusCode::OK);
        serde_json::from_slice::<Value>(&answer.body).unwrap()["methodResponses"].as_array().unwrap().clone()
    }

    async fn put_card(&self, login: &str, path: &str, card: String) -> Answer {
        self.send(Some(login), "PUT", path, &[("content-type", "text/vcard")], card.into_bytes()).await
    }

    /// A GET without a login, as Libravatar clients make it.
    async fn public(&self, path: String) -> Answer {
        self.send(None, "GET", &path, &[], Vec::new()).await
    }

    fn asked(&self) -> Vec<String> {
        self.net.asked.lock().unwrap().clone()
    }
}

fn kind(answer: &Answer) -> &str {
    answer.headers.get("x-picture-kind").and_then(|v| v.to_str().ok()).unwrap_or_default()
}

fn cache_control(answer: &Answer) -> &str {
    answer.headers.get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

fn data_uri(bytes: &[u8]) -> String {
    use base64::Engine;
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn vcard(uid: &str, email: &str, photo: Option<&str>) -> String {
    let photo = photo.map(|photo| format!("PHOTO:{photo}\r\n")).unwrap_or_default();
    format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{uid}\r\nEMAIL:{email}\r\n{photo}END:VCARD\r\n")
}

async fn stored(store: &uwumail_store::Store, owner: PictureOwner, size: u32) -> Vec<u8> {
    let prepared = prepare(&sample(size, size, "png")).unwrap();
    let picture =
        NewPicture { bytes: prepared.bytes.clone(), media_type: prepared.media_type.into(), face: Some(prepared.face) };
    store.set_picture(owner, Some(picture)).await.unwrap();
    prepared.bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sender_picture_comes_from_the_first_place_that_has_one() {
    let libravatar_picture = sample(128, 128, "png");
    let mut net = FakeNet::default();
    net.srv.insert("_avatars-sec._tcp.elsewhere.example".into(), vec![(0, 0, 443, "avatars.elsewhere.example".into())]);
    let url = format!("https://avatars.elsewhere.example:443/avatar/{}?s=128&d=404", libravatar_hash(FRIEND));
    net.pages.insert(url.clone(), libravatar_picture.clone());
    let pictures = pictures(net).await;
    let mini = "mini@example.org";

    // Without the network nothing is known yet, and nobody is asked.
    assert_eq!(pictures.picture(mini, FRIEND, "&local=1").await.status, StatusCode::NOT_FOUND);
    assert!(pictures.asked().is_empty());

    // d. Libravatar, where the domain publishes it.
    let found = pictures.picture(mini, FRIEND, "").await;
    assert_eq!((found.status, kind(&found)), (StatusCode::OK, "photo"));
    assert_eq!(found.body, libravatar_picture);
    assert_eq!(cache_control(&found), "private, no-cache");
    let etag = found.headers.get(header::ETAG).unwrap().to_str().unwrap().to_owned();
    assert_eq!(pictures.asked(), vec!["_avatars-sec._tcp.elsewhere.example".to_owned(), url]);
    let again = pictures
        .send(
            Some(mini),
            "GET",
            &format!("/jmap/picture/{}?email=friend%40elsewhere.example", pictures.server.account_id(mini).await),
            &[("if-none-match", &etag)],
            Vec::new(),
        )
        .await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    // Known now, so also without the network — and nobody is asked twice.
    assert_eq!(pictures.picture(mini, FRIEND, "&local=1").await.body, libravatar_picture);
    assert_eq!(pictures.picture("nyu@example.org", FRIEND, "").await.status, StatusCode::OK);
    assert_eq!(pictures.asked().len(), 2, "one lookup for the whole server");
    // A domain without the record gets asked for nothing but the record, once.
    assert_eq!(pictures.picture(mini, "someone@nowhere.example", "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(pictures.picture(mini, "other@nowhere.example", "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(pictures.asked().len(), 4);

    // c. A Face that came with mail that passed DMARC is newer news than Libravatar.
    let face = sample(48, 48, "png");
    pictures.server.store.store_received_face(FRIEND, face.clone()).await.unwrap();
    let found = pictures.picture(mini, FRIEND, "&local=1").await;
    assert_eq!((found.body == face, kind(&found)), (true, "photo"));

    // a. The reader's own contact photo beats everything.
    let photo = prepare(&sample(90, 90, "jpeg")).unwrap().bytes;
    let account = pictures.server.account_id(mini).await;
    let responses = pictures
        .api(
            mini,
            json!([["ContactCard/set", { "accountId": account, "create": { "c": {
            "name": { "full": "Friend" },
            "emails": { "e": { "address": "Friend@Elsewhere.example" } },
            "media": { "p": { "kind": "photo", "uri": data_uri(&photo) } }
        } } }, "0"]]),
        )
        .await;
    assert!(responses[0][1]["created"]["c"].is_object(), "{}", responses[0][1]);
    let found = pictures.picture(mini, FRIEND, "").await;
    assert_eq!((found.status, found.body == photo), (StatusCode::OK, true));
    assert_eq!(found.headers.get(header::CONTENT_TYPE).unwrap(), "image/jpeg");
    // Only for the one whose card it is.
    assert_eq!(pictures.picture("nyu@example.org", FRIEND, "&local=1").await.body, face);

    // `source=logo` skips the people; elsewhere.example has no company logo to find.
    assert_eq!(pictures.picture(mini, FRIEND, "&source=logo&local=1").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn people_here_logos_and_masked_addresses() {
    let pictures = pictures(FakeNet::default()).await;
    let store = &pictures.server.store;
    let mini = pictures.server.id("mini@example.org").await;
    let nyu = "nyu@example.org";
    let own = stored(store, PictureOwner::Account(mini), 200).await;
    let domain = store.domain("example.org").await.unwrap().unwrap().id;

    // b. Someone here: for everyone here, their sub-addresses too.
    let found = pictures.picture(nyu, "Mini@example.org", "").await;
    assert_eq!((found.status, kind(&found), found.body == own), (StatusCode::OK, "photo", true));
    assert_eq!(pictures.picture(nyu, "mini+news@example.org", "").await.body, own);

    // e. The domain's logo, for `source=logo` and for someone who keeps their picture to themselves.
    let logo = stored(store, PictureOwner::Domain(domain), 64).await;
    let found = pictures.picture(nyu, "mini@example.org", "&source=logo").await;
    assert_eq!((kind(&found), found.body == logo), ("logo", true));
    assert_eq!(cache_control(&found), "private, max-age=86400");
    store
        .update_profile(mini, ProfileUpdate { visibility: Some(PictureVisibility::Off), ..Default::default() })
        .await
        .unwrap();
    let found = pictures.picture(nyu, "mini@example.org", "").await;
    assert_eq!((kind(&found), found.body == logo), ("logo", true));
    assert_eq!(pictures.picture(nyu, "nobody@example.org", "").await.body, logo);
    // A Face kept from mail of one of our own addresses does not outlast its owner's choice.
    store.store_received_face("mini@example.org", sample(48, 48, "png")).await.unwrap();
    let found = pictures.picture(nyu, "mini@example.org", "").await;
    assert_eq!((kind(&found), found.body == logo), ("logo", true));

    // A masked address shows nothing of whose it is: no picture, no logo, no Face, no lookup.
    store
        .set_domain_masked_policy("example.org", DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() })
        .await
        .unwrap();
    store
        .update_profile(mini, ProfileUpdate { visibility: Some(PictureVisibility::Server), ..Default::default() })
        .await
        .unwrap();
    let masked = store
        .create_masked_address(
            mini,
            NewMaskedAddress {
                domain: None,
                state: Some(MaskedState::Enabled),
                for_domain: "https://shop.example.com".into(),
                description: String::new(),
                url: None,
                email_prefix: None,
                created_by: "test".into(),
            },
        )
        .await
        .unwrap();
    store.store_received_face(&masked.email, sample(48, 48, "png")).await.unwrap();
    assert_eq!(pictures.picture(nyu, &masked.email, "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(pictures.picture(nyu, &masked.email, "&source=logo").await.status, StatusCode::NOT_FOUND);
    assert!(pictures.asked().is_empty());
    // Only the reader's own card can give it a picture.
    let book = "/dav/addressbooks/nyu@example.org/contacts/";
    let photo = prepare(&sample(40, 40, "jpeg")).unwrap().bytes;
    let put = pictures
        .send(
            Some(nyu),
            "PUT",
            &format!("{book}shop.vcf"),
            &[("content-type", "text/vcard")],
            vcard("shop", &masked.email, Some(&data_uri(&photo))).into_bytes(),
        )
        .await;
    assert_eq!(put.status, StatusCode::CREATED);
    assert_eq!(pictures.picture(nyu, &masked.email, "").await.body, photo);

    // Someone else's account is not the caller's.
    let other = pictures.server.account_id("mini@example.org").await;
    let answer = pictures
        .send(Some(nyu), "GET", &format!("/jmap/picture/{other}?email=mini%40example.org"), &[], Vec::new())
        .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_contact_index_follows_carddav_jmap_and_sharing() {
    let pictures = pictures(FakeNet::default()).await;
    let mini = "mini@example.org";
    let nyu = "nyu@example.org";
    let book = "/dav/addressbooks/mini@example.org/contacts/";
    let photo = prepare(&sample(40, 40, "jpeg")).unwrap().bytes;
    let status = |answer: Answer| answer.status;

    // CardDAV: a card with a photo, then without, then gone.
    let ami = format!("{book}ami.vcf");
    let put = |card: String| pictures.put_card(mini, &ami, card);
    assert_eq!(put(vcard("ami", "ami@elsewhere.example", Some(&data_uri(&photo)))).await.status, StatusCode::CREATED);
    assert_eq!(pictures.picture(mini, "ami@elsewhere.example", "&local=1").await.body, photo);
    assert_eq!(status(put(vcard("ami", "ami@elsewhere.example", None)).await), StatusCode::NO_CONTENT);
    assert_eq!(status(pictures.picture(mini, "ami@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);
    put(vcard("ami", "ami@elsewhere.example", Some(&data_uri(&photo)))).await;
    assert_eq!(
        status(pictures.send(Some(mini), "DELETE", &format!("{book}ami.vcf"), &[], Vec::new()).await),
        StatusCode::NO_CONTENT
    );
    assert_eq!(status(pictures.picture(mini, "ami@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);

    // JMAP: created with a photo, the photo taken away, then back and the card destroyed.
    let account = pictures.server.account_id(mini).await;
    let created = pictures
        .api(
            mini,
            json!([["ContactCard/set", { "accountId": account, "create": { "c": {
            "name": { "full": "Uwe" },
            "emails": { "e": { "address": "uwe@elsewhere.example" } },
            "media": { "p": { "kind": "photo", "uri": data_uri(&photo) } }
        } } }, "0"]]),
        )
        .await;
    let id = created[0][1]["created"]["c"]["id"].as_str().unwrap().to_owned();
    assert_eq!(pictures.picture(mini, "uwe@elsewhere.example", "&local=1").await.body, photo);
    pictures
        .api(mini, json!([["ContactCard/set", { "accountId": account, "update": { &id: { "media": null } } }, "0"]]))
        .await;
    assert_eq!(status(pictures.picture(mini, "uwe@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);
    pictures
        .api(mini, json!([["ContactCard/set", { "accountId": account, "update": { &id: { "media": { "p": { "kind": "photo", "uri": data_uri(&photo) } } } } }, "0"]]))
        .await;
    assert_eq!(pictures.picture(mini, "uwe@elsewhere.example", "&local=1").await.body, photo);
    pictures.api(mini, json!([["ContactCard/set", { "accountId": account, "destroy": [id] }, "0"]])).await;
    assert_eq!(status(pictures.picture(mini, "uwe@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);

    // An address book shared with someone gives them its photos too, and only them.
    put(vcard("ami", "ami@elsewhere.example", Some(&data_uri(&photo)))).await;
    assert_eq!(status(pictures.picture(nyu, "ami@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);
    let store = &pictures.server.store;
    let mini_id = pictures.server.id(mini).await;
    let books = store
        .dav_collections(
            mini_id,
            uwumail_store::DavKind::Addressbook,
            uwumail_store::NewDavCollection::default_address_book("Kontakte"),
        )
        .await
        .unwrap();
    store.dav_share(mini_id, books[0].id, nyu, ShareRights::Read).await.unwrap();
    assert_eq!(pictures.picture(nyu, "ami@elsewhere.example", "&local=1").await.body, photo);

    // A linked photo is fetched through the egress, and only over https.
    let linked = "https://photos.elsewhere.example/ami.jpg";
    assert_eq!(put(vcard("ami", "ami@elsewhere.example", Some(linked))).await.status, StatusCode::NO_CONTENT);
    assert_eq!(status(pictures.picture(mini, "ami@elsewhere.example", "&local=1").await), StatusCode::NOT_FOUND);
    assert!(pictures.asked().is_empty(), "local=1 asks nobody");
    assert_eq!(status(pictures.picture(mini, "ami@elsewhere.example", "").await), StatusCode::NOT_FOUND);
    assert!(pictures.asked().contains(&linked.to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn libravatar_answers_for_public_pictures_only() {
    let pictures = pictures(FakeNet::default()).await;
    let store = &pictures.server.store;
    let mini = pictures.server.id("mini@example.org").await;
    store.add_alias("hello@example.org", "mini@example.org").await.unwrap();
    stored(store, PictureOwner::Account(mini), 300).await;
    let [md5, sha256] = libravatar_hashes("Mini@Example.org");
    let get = |path: String| pictures.public(path);

    // Server-only pictures stay on the server.
    assert_eq!(get(format!("/avatar/{sha256}?d=404")).await.status, StatusCode::NOT_FOUND);
    store
        .update_profile(mini, ProfileUpdate { visibility: Some(PictureVisibility::Public), ..Default::default() })
        .await
        .unwrap();

    let found = get(format!("/avatar/{md5}")).await;
    assert_eq!(
        (found.status, found.headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap()),
        (StatusCode::OK, "image/jpeg")
    );
    let decoded = image_size(&found.body);
    assert_eq!(decoded, 80, "80 without s=");
    assert_eq!(image_size(&get(format!("/avatar/{sha256}?s=32")).await.body), 32);
    assert_eq!(
        image_size(&get(format!("/avatar/{}?size=2000", sha256.to_uppercase())).await.body),
        300,
        "never scaled up"
    );
    let [_, alias] = libravatar_hashes("hello@example.org");
    assert_eq!(get(format!("/avatar/{alias}")).await.status, StatusCode::OK);

    // Unknown hashes get what d= asks for.
    let unknown = "0".repeat(64);
    assert_eq!(get(format!("/avatar/{unknown}?d=404")).await.status, StatusCode::NOT_FOUND);
    let silhouette = get(format!("/avatar/{unknown}?s=40")).await;
    assert_eq!((silhouette.status, image_size(&silhouette.body)), (StatusCode::OK, 40), "mm without d=");
    assert_eq!(image_size(&get(format!("/avatar/{unknown}?default=blank&s=20")).await.body), 20);
    let moved = get(format!("/avatar/{unknown}?d=https%3A%2F%2Fpictures.example.com%2Fx.png")).await;
    assert_eq!(
        (moved.status, moved.headers.get(header::LOCATION).unwrap().to_str().unwrap()),
        (StatusCode::FOUND, "https://pictures.example.com/x.png")
    );
    assert_eq!(
        get(format!("/avatar/{unknown}?d=http%3A%2F%2Fpictures.example.com%2Fx.png")).await.status,
        StatusCode::OK,
        "only https is followed"
    );
    assert_eq!(get("/avatar/not-a-hash".into()).await.status, StatusCode::BAD_REQUEST);

    // A masked address is never in the table.
    store
        .set_domain_masked_policy("example.org", DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() })
        .await
        .unwrap();
    let masked = store
        .create_masked_address(
            mini,
            NewMaskedAddress {
                domain: None,
                state: Some(MaskedState::Enabled),
                for_domain: "https://shop.example.com".into(),
                description: String::new(),
                url: None,
                email_prefix: None,
                created_by: "test".into(),
            },
        )
        .await
        .unwrap();
    let [_, hidden] = libravatar_hashes(&masked.email);
    assert_eq!(get(format!("/avatar/{hidden}?d=404")).await.status, StatusCode::NOT_FOUND);

    // Forbidden for the domain: gone at once.
    let domain = store.domain("example.org").await.unwrap().unwrap().id;
    store.set_domain_public_pictures(domain, false).await.unwrap();
    assert_eq!(get(format!("/avatar/{sha256}?d=404")).await.status, StatusCode::NOT_FOUND);
    store.set_domain_public_pictures(domain, true).await.unwrap();
    assert_eq!(get(format!("/avatar/{sha256}?d=404")).await.status, StatusCode::OK);

    // One network asking again and again is slowed down.
    let mut limited = false;
    for _ in 0..130 {
        if get(format!("/avatar/{unknown}?d=404")).await.status == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited);
}

/// The width of a PNG or JPEG.
fn image_size(bytes: &[u8]) -> u32 {
    if bytes.starts_with(b"\x89PNG") {
        return u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    }
    // JPEG: the first start-of-frame marker holds height and width.
    let mut at = 2;
    while at + 9 < bytes.len() {
        let marker = bytes[at + 1];
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if (0xc0..=0xc3).contains(&marker) {
            return u16::from_be_bytes([bytes[at + 7], bytes[at + 8]]) as u32;
        }
        at += 2 + length;
    }
    panic!("not a picture");
}
