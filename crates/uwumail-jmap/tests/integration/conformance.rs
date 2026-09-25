//! RFC 8620/8621 details that third-party clients rely on, found by running Fastmail's
//! JMAP-TestSuite and aerc against the server (docs/jmap-clients.md).

use crate::common;

use common::server;
use serde_json::{Value, json};

const MINI: &str = "mini@example.org";

async fn call(server: &common::Server, method: &str, arguments: Value) -> Value {
    let responses = server.api(MINI, json!([[method, arguments, "0"]])).await;
    responses[0][1].clone()
}

async fn create_mailbox(server: &common::Server, account: &str, name: &str, parent: Option<&str>) -> String {
    let result = call(
        server,
        "Mailbox/set",
        json!({ "accountId": account, "create": { "m": { "name": name, "parentId": parent } } }),
    )
    .await;
    result["created"]["m"]["id"].as_str().unwrap_or_else(|| panic!("{result}")).to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn mailbox_query_has_operators_paging_and_trees() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let aaa = create_mailbox(&server, &account, "aaa", None).await;
    let bbb = create_mailbox(&server, &account, "bbb", Some(&aaa)).await;
    let ccc = create_mailbox(&server, &account, "ccc", None).await;
    let query = |extra: Value| {
        let mut arguments = json!({ "accountId": account, "sort": [{ "property": "name" }] });
        arguments.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        call(&server, "Mailbox/query", arguments)
    };

    // Operators, not only a single condition.
    let result = query(json!({ "filter": { "operator": "AND", "conditions": [
        { "hasAnyRole": false }, { "parentId": null }
    ] } }))
    .await;
    assert_eq!(result["ids"], json!([aaa, ccc]), "{result}");
    let result = query(json!({ "filter": { "operator": "OR", "conditions": [
        { "parentId": aaa }, { "name": "ccc" }
    ] } }))
    .await;
    assert_eq!(result["ids"], json!([bbb, ccc]), "{result}");
    let result = query(json!({ "filter": { "operator": "NOT", "conditions": [{ "hasAnyRole": true }] } })).await;
    assert_eq!(result["ids"], json!([aaa, bbb, ccc]), "{result}");

    // Paging like every other /query.
    let own = json!({ "filter": { "hasAnyRole": false } });
    let page = |extra: Value| {
        let mut arguments = own.clone();
        arguments.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        query(arguments)
    };
    let result = page(json!({ "position": 1, "limit": 1, "calculateTotal": true })).await;
    assert_eq!(result["ids"], json!([bbb]), "{result}");
    assert_eq!(result["position"], 1);
    assert_eq!(result["total"], 3);
    let result = page(json!({ "position": -1 })).await;
    assert_eq!(result["ids"], json!([ccc]), "{result}");
    assert!(result.get("total").is_none(), "total only when asked for");
    let result = page(json!({ "anchor": bbb, "anchorOffset": -1, "limit": 2 })).await;
    assert_eq!(result["ids"], json!([aaa, bbb]), "{result}");
    let result = page(json!({ "limit": -1 })).await;
    assert_eq!(result["type"], "invalidArguments", "{result}");
    let result = page(json!({ "anchor": "m999999" })).await;
    assert_eq!(result["type"], "anchorNotFound", "{result}");

    // Descending, and as a tree: parents before their children.
    let result = page(json!({ "sort": [{ "property": "name", "isAscending": false }] })).await;
    assert_eq!(result["ids"], json!([ccc, bbb, aaa]), "{result}");
    let result = page(json!({ "sort": [{ "property": "name", "isAscending": false }], "sortAsTree": true })).await;
    assert_eq!(result["ids"], json!([ccc, aaa, bbb]), "{result}");
    // filterAsTree: a child whose parent does not match is left out.
    let result = query(json!({ "filter": { "name": "bb" }, "filterAsTree": true })).await;
    assert_eq!(result["ids"], json!([]), "{result}");
    let result = query(json!({ "filter": { "name": "bb" } })).await;
    assert_eq!(result["ids"], json!([bbb]), "{result}");

    let result = query(json!({ "filter": { "operator": "XOR", "conditions": [] } })).await;
    assert_eq!(result["type"], "unsupportedFilter", "{result}");
    let result = query(json!({ "filter": { "colour": "blue" } })).await;
    assert_eq!(result["type"], "unsupportedFilter", "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_set_mailbox_properties_may_come_back_unchanged() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let id = create_mailbox(&server, &account, "Rezepte", None).await;

    // A client that sends back the whole object from Mailbox/get, with a new name.
    let got = call(&server, "Mailbox/get", json!({ "accountId": account, "ids": [id] })).await;
    let mut mailbox = got["list"][0].clone();
    mailbox["name"] = json!("Kochbuch");
    let result =
        call(&server, "Mailbox/set", json!({ "accountId": account, "update": { id.clone(): mailbox.clone() } })).await;
    assert!(result["updated"].get(&id).is_some(), "{result}");
    let got = call(&server, "Mailbox/get", json!({ "accountId": account, "ids": [id] })).await;
    assert_eq!(got["list"][0]["name"], "Kochbuch");

    // The same object without its id creates a copy.
    let mut copy = mailbox.clone();
    copy.as_object_mut().unwrap().remove("id");
    copy["name"] = json!("Kochbuch 2");
    let result = call(&server, "Mailbox/set", json!({ "accountId": account, "create": { "c": copy } })).await;
    assert!(result["created"]["c"]["id"].is_string(), "{result}");

    // Changed values are refused, all of them named.
    let result = call(
        &server,
        "Mailbox/set",
        json!({ "accountId": account, "update": { id.clone(): {
            "id": "m1", "totalEmails": 52, "myRights": { "mayDelete": false }, "name": "Nope"
        } } }),
    )
    .await;
    let error = &result["notUpdated"][&id];
    assert_eq!(error["type"], "invalidProperties", "{result}");
    let mut properties: Vec<&str> =
        error["properties"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    properties.sort_unstable();
    assert!(properties.starts_with(&["id", "myRights/mayAddItems"]), "{properties:?}");
    assert!(properties.contains(&"myRights/mayDelete") && properties.contains(&"totalEmails"), "{properties:?}");
    let got = call(&server, "Mailbox/get", json!({ "accountId": account, "ids": [id] })).await;
    assert_eq!(got["list"][0]["name"], "Kochbuch", "nothing of a refused update is applied");

    let result = call(
        &server,
        "Mailbox/set",
        json!({ "accountId": account, "create": { "c": { "name": "Mit Id", "id": id, "unreadEmails": 3 } } }),
    )
    .await;
    let mut properties: Vec<&str> =
        result["notCreated"]["c"]["properties"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    properties.sort_unstable();
    assert_eq!(properties, ["id", "unreadEmails"], "{result}");
}

const GROUPS_AND_DATES: &str = "From: Nyu <nyu@example.org>
To: mini@example.org
X-Group: A group: one@example.org, Two <two@example.org>;, three@example.org
Date: Thu, 13 Feb 1969 23:32:00 -0330
Subject: Zeilen
Content-Type: multipart/mixed; boundary=b

--b
Content-Type: text/plain

erste Zeile
zweite Zeile
--b
Content-Type: application/octet-stream

AAAA
--b--
";

#[tokio::test(flavor = "multi_thread")]
async fn email_get_follows_the_details_of_rfc_8621() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let id = server.deliver(MINI, GROUPS_AND_DATES).await;
    let result = call(
        &server,
        "Email/get",
        json!({ "accountId": account, "ids": [id], "fetchAllBodyValues": true,
            "properties": ["to", "from", "bodyValues", "textBody", "bodyStructure",
                "header:X-Group:asGroupedAddresses", "header:Date:asDate"],
            "bodyProperties": ["partId", "type", "charset", "subParts"] }),
    )
    .await;
    let email = &result["list"][0];
    // Every EmailAddress has a name, null when there is none.
    assert_eq!(email["to"], json!([{ "name": null, "email": "mini@example.org" }]), "{email}");
    // Body values have LF line endings.
    let text_part = email["textBody"][0]["partId"].as_str().unwrap();
    assert_eq!(email["bodyValues"][text_part]["value"], "erste Zeile\nzweite Zeile", "{email}");
    // A text part without a charset parameter is US-ASCII; other parts have no charset.
    assert_eq!(email["textBody"][0]["charset"], "us-ascii", "{email}");
    assert_eq!(email["bodyStructure"]["subParts"][1]["charset"], Value::Null);
    // subParts is null for a leaf part that was asked for it.
    assert_eq!(email["textBody"][0]["subParts"], Value::Null);
    assert!(email["textBody"][0].as_object().unwrap().contains_key("subParts"));
    assert_eq!(email["bodyStructure"]["subParts"].as_array().map(Vec::len), Some(2));
    // Groups keep their names; addresses outside a group are a group without one.
    assert_eq!(
        email["header:X-Group:asGroupedAddresses"],
        json!([
            { "name": "A group", "addresses": [
                { "name": null, "email": "one@example.org" }, { "name": "Two", "email": "two@example.org" }
            ] },
            { "name": null, "addresses": [{ "name": null, "email": "three@example.org" }] }
        ]),
        "{email}"
    );
    // A Date keeps its offset.
    assert_eq!(email["header:Date:asDate"], "1969-02-13T23:32:00-03:30");

    for invalid in [json!(-5), json!("cat"), json!("1"), json!({}), json!([]), json!(true)] {
        let result = call(
            &server,
            "Email/get",
            json!({ "accountId": account, "ids": [id], "properties": ["bodyValues"], "maxBodyValueBytes": invalid }),
        )
        .await;
        assert_eq!(result["type"], "invalidArguments", "{invalid}: {result}");
    }
    let result = call(
        &server,
        "Email/get",
        json!({ "accountId": account, "ids": [id], "properties": ["bodyValues"], "fetchTextBodyValues": true,
            "maxBodyValueBytes": 5 }),
    )
    .await;
    assert_eq!(
        result["list"][0]["bodyValues"][text_part],
        json!({
            "value": "erste", "isTruncated": true, "isEncodingProblem": false
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn email_set_creates_every_header_form_and_checks_the_rules() {
    let server = server().await;
    let account = server.account_id(MINI).await;
    let drafts = server.mailbox(MINI, "drafts").await;
    let create = |object: Value| {
        let mut object = object;
        object["mailboxIds"] = json!({ drafts.clone(): true });
        call(&server, "Email/set", json!({ "accountId": account, "create": { "new": object } }))
    };

    let result = create(json!({
        "header:X-Tier:all": ["cat", "dog"],
        "header:Sender:asAddresses": [{ "name": "Foo bar", "email": "foo@example.org" }],
        "header:X-Crew:asGroupedAddresses": [{ "name": "Crew", "addresses": [{ "name": null, "email": "a@example.org" }] }],
        "header:Message-ID:asMessageIds": ["one@example.org"],
        "header:Date:asDate": "2026-09-14T10:00:00+02:00",
        "header:List-Help:asURLs": ["https://example.org/help"],
        "header:Subject:asText": "Grüße",
        "bodyValues": { "t": { "value": "Hallo" } },
        "textBody": [{ "partId": "t", "header:X-Part": " yes", "language": ["de"] }]
    }))
    .await;
    let id = result["created"]["new"]["id"].as_str().unwrap_or_else(|| panic!("{result}")).to_owned();
    let result = call(
        &server,
        "Email/get",
        json!({ "accountId": account, "ids": [id], "bodyProperties": ["language", "header:X-Part"],
            "properties": ["header:X-Tier:all", "sender", "header:X-Crew:asGroupedAddresses", "messageId",
                "sentAt", "header:List-Help:asURLs", "subject", "header:Date:all", "textBody"] }),
    )
    .await;
    let email = &result["list"][0];
    assert_eq!(email["header:X-Tier:all"], json!([" cat", " dog"]), "{email}");
    assert_eq!(email["sender"], json!([{ "name": "Foo bar", "email": "foo@example.org" }]));
    assert_eq!(email["header:X-Crew:asGroupedAddresses"][0]["name"], "Crew");
    assert_eq!(email["messageId"], json!(["one@example.org"]));
    assert_eq!(email["sentAt"], "2026-09-14T08:00:00Z");
    assert_eq!(email["header:Date:all"].as_array().map(Vec::len), Some(1), "one Date only: {email}");
    assert_eq!(email["header:List-Help:asURLs"], json!(["https://example.org/help"]));
    assert_eq!(email["subject"], "Grüße");
    assert_eq!(email["textBody"][0]["language"], json!(["de"]), "{email}");
    assert_eq!(email["textBody"][0]["header:X-Part"], " yes", "{email}");

    let text = json!({ "bodyValues": { "t": { "value": "Hallo" } }, "textBody": [{ "partId": "t" }] });
    let with = |extra: Value| {
        let mut object = text.clone();
        object.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        object
    };
    let refused = [
        (with(json!({ "header:X-Tier": ["cat", "dog"] })), vec!["header:X-Tier"]),
        (with(json!({ "headers": [{ "name": "X-A", "value": "b" }] })), vec!["headers"]),
        (with(json!({ "header:Content-Type": "text/plain" })), vec!["header:Content-Type"]),
        (with(json!({ "header:From:asDate": "2026-09-14T10:00:00Z" })), vec!["header:From:asDate"]),
        (with(json!({ "from": [{ "email": "a@example.org" }], "header:from": " b@example.org" })), vec!["header:from"]),
        (
            with(json!({ "textBody": [{ "partId": "t", "header:Content-Transfer-Encoding": "base64" }] })),
            vec!["textBody/0/header:Content-Transfer-Encoding"],
        ),
        (with(json!({ "textBody": [{ "partId": "t" }, { "partId": "t" }] })), vec!["textBody"]),
        (with(json!({ "textBody": [{ "partId": "t", "type": "text/html" }] })), vec!["textBody"]),
        (with(json!({ "textBody": [{ "partId": "t", "charset": "latin1" }] })), vec!["textBody/0/charset"]),
        (
            with(json!({ "textBody": [{ "partId": "t", "blobId": "b1" }] })),
            vec!["textBody/0/partId", "textBody/0/blobId"],
        ),
        (
            with(json!({ "bodyValues": { "t": { "value": "Hallo", "isTruncated": true } } })),
            vec!["bodyValues/t/isTruncated"],
        ),
        (
            with(json!({ "bodyStructure": { "partId": "t" }, "htmlBody": [{ "partId": "t" }] })),
            vec!["textBody", "htmlBody"],
        ),
    ];
    for (object, properties) in refused {
        let result = create(object.clone()).await;
        let error = &result["notCreated"]["new"];
        assert_eq!(error["type"], "invalidProperties", "{object}: {result}");
        assert_eq!(error["properties"], json!(properties), "{object}: {result}");
    }

    let result = create(json!({ "textBody": [{ "blobId": "cat" }], "attachments": [{ "blobId": "dog" }] })).await;
    assert_eq!(result["notCreated"]["new"]["type"], "blobNotFound", "{result}");
    assert_eq!(result["notCreated"]["new"]["notFound"], json!(["cat", "dog"]), "{result}");

    let result = call(
        &server,
        "Email/import",
        json!({ "accountId": account, "emails": { "a": {}, "b": {
        "blobId": "nope", "mailboxIds": { drafts.clone(): true }
    } } }),
    )
    .await;
    assert_eq!(result["notCreated"]["a"]["properties"], json!(["blobId", "mailboxIds"]), "{result}");
    assert_eq!(result["notCreated"]["b"]["type"], "blobNotFound", "{result}");
    assert_eq!(result["notCreated"]["b"]["notFound"], json!(["nope"]), "{result}");
}
