//! RFC 8620/8621 details that third-party clients rely on, found by running Fastmail's
//! JMAP-TestSuite and aerc against the server (docs/jmap-clients.md).

mod common;

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
