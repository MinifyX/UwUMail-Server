//! What the AI assistant costs in the portal: prices on the admin's providers, the show-cost switch,
//! and costs in the admin's statistics and a person's usage, in the currency asked for.

use std::collections::BTreeMap;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uwumail_assist::{Assist, PriceTable, chatgpt};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{Smtp, SmtpSettings};
use uwumail_store::{NewAccount, Role, Store};
use uwumail_web::{CSRF_HEADER, Web, WebSettings};

async fn portal() -> (Router, Store, i64, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let mut mini = 0;
    for (user, role) in [("nyu", Role::Admin), ("mini", Role::User)] {
        mini = store
            .create_account(NewAccount {
                address: format!("{user}@example.org"),
                display_name: user.into(),
                password: Some("katzenpfote-123".into()),
                role,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
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
    let assist = Assist::for_tests(store.clone(), "mail.example.org", chatgpt::Endpoints::default());
    let rates = BTreeMap::from([("USD".to_owned(), 1.25), ("JPY".to_owned(), 160.0)]);
    let mut rates_of_mini = uwumail_assist::Rates::plain(0.00000025, 0.000002);
    rates_of_mini.reasoning_model = true;
    rates_of_mini.max_output_tokens = Some(128_000);
    let models = BTreeMap::from([("gpt-5-mini".to_owned(), rates_of_mini)]);
    let table = PriceTable {
        fetched_at: 1_790_000_000,
        models,
        rates,
        rates_day: Some("2026-09-29".into()),
        ..PriceTable::default()
    };
    assist.set_prices(table).await.unwrap();
    web.set_assist(assist);
    (web.router(), store, mini, dir)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: &(String, String),
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, &auth.0)
        .header(CSRF_HEADER, &auth.1)
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ClientInfo { https: true, ..ClientInfo::default() })
        .body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn login(app: &Router, user: &str) -> (String, String) {
    let body = json!({ "login": format!("{user}@example.org"), "password": "katzenpfote-123" });
    let request = Request::post("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ClientInfo { https: true, ..ClientInfo::default() })
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let cookie = response.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap();
    let cookie = cookie.to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (cookie, json["csrfToken"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn prices_and_costs_in_the_portal() {
    let (app, store, account, _dir) = portal().await;
    let admin = login(&app, "nyu").await;
    let person = login(&app, "mini").await;

    let body = json!({ "kind": "openai", "apiKey": "sk-test-abcdefgh1234", "model": "gpt-5-mini" });
    let (status, created) = call(&app, "POST", "/api/admin/assist/providers", Some(body), &admin).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["showCostToUsers"], false);
    let price = &created["price"];
    assert_eq!((&price["inputPerMillion"], &price["outputPerMillion"]), (&json!(0.25), &json!(2.0)), "{price}");
    assert_eq!((&price["reasoningPerMillion"], &price["supportsReasoning"]), (&json!(2.0), &json!(true)));
    assert_eq!(price["source"], "auto");
    let id = created["id"].as_i64().unwrap();

    let (_, view) = call(&app, "GET", "/api/admin/assist", None, &admin).await;
    assert_eq!(view["priceLists"]["models"], 1);
    assert_eq!(view["priceLists"]["ratesDay"], "2026-09-29");

    // What mini used today: 1,000,000 tokens in and 100,000 out, 0.45 US dollars.
    let day = store.reserve_assist_usage(account, id, "summarize", None, None).await.unwrap();
    let count = uwumail_store::TokenCount {
        input: 1_000_000,
        output: 100_000,
        calls: 1,
        cost_usd: Some(0.45),
        ..Default::default()
    };
    store.add_assist_tokens(account, id, day, "summarize", count).await.unwrap();

    let (status, usage) = call(&app, "GET", "/api/admin/assist/usage?days=1&currency=JPY", None, &admin).await;
    assert_eq!(status, StatusCode::OK, "{usage}");
    assert_eq!((&usage["days"][0]["reasoningTokens"], &usage["days"][0]["calls"]), (&json!(0), &json!(1)));
    let cost = &usage["days"][0]["cost"];
    assert_eq!((cost["currency"].as_str(), cost["usd"].as_f64()), (Some("JPY"), Some(0.45)));
    assert!((cost["amount"].as_f64().unwrap() - 57.6).abs() < 1e-9, "{cost}");
    let (status, _) = call(&app, "GET", "/api/admin/assist/usage?currency=yen", None, &admin).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // The person sees the cost only once the admin shows it.
    let (_, usage) = call(&app, "GET", "/api/account/assist/usage?days=1", None, &person).await;
    assert_eq!((&usage["days"][0]["cost"], &usage["today"][0]["cost"]), (&Value::Null, &Value::Null), "{usage}");
    let (_, mine) = call(&app, "GET", "/api/account/assist", None, &person).await;
    assert_eq!(mine["providers"][0]["price"], Value::Null);

    let body = json!({ "showCostToUsers": true, "inputPricePerMillion": 1.0 });
    let (status, updated) = call(&app, "PATCH", &format!("/api/admin/assist/providers/{id}"), Some(body), &admin).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["price"],
        json!({
            "inputPerMillion": 1.0, "outputPerMillion": 2.0, "reasoningPerMillion": 2.0,
            "cacheReadPerMillion": 1.0, "cacheWritePerMillion": 1.0, "perRequest": 0.0, "perImage": 0.0,
            "webSearchPerQuery": 0.0, "tiers": [], "supportsReasoning": true, "maxOutputTokens": 128000,
            "source": "manual"
        })
    );
    let body = json!({ "pricePerRequest": 0.002 });
    let (_, updated) = call(&app, "PATCH", &format!("/api/admin/assist/providers/{id}"), Some(body), &admin).await;
    assert_eq!((&updated["pricePerRequest"], &updated["price"]["perRequest"]), (&json!(0.002), &json!(0.002)));
    let body = json!({ "pricePerRequest": 1000 });
    let (status, _) = call(&app, "PATCH", &format!("/api/admin/assist/providers/{id}"), Some(body), &admin).await;
    assert_eq!(status, StatusCode::CONFLICT, "at most 100 US dollars a request");
    let (_, usage) = call(&app, "GET", "/api/account/assist/usage?days=1", None, &person).await;
    let cost = &usage["days"][0]["cost"];
    assert_eq!(cost["currency"], "EUR");
    assert!((cost["amount"].as_f64().unwrap() - 0.36).abs() < 1e-9, "{cost}");
    assert_eq!(usage["today"][0]["cost"]["usd"], 0.45);
    let (_, mine) = call(&app, "GET", "/api/account/assist", None, &person).await;
    assert_eq!(mine["providers"][0]["price"]["source"], "manual");

    // Someone reading in English may take US dollars; it is the synced user setting assist.currency.
    let (status, settings) =
        call(&app, "PUT", "/api/account/assist/settings", Some(json!({ "currency": "USD" })), &person).await;
    assert_eq!((status, &settings["currency"]), (StatusCode::OK, &json!("USD")), "{settings}");
    let values = store.user_settings(account).await.unwrap().values;
    assert_eq!(values.get("assist.currency"), Some(&json!("USD")));
    let (status, _) =
        call(&app, "PUT", "/api/account/assist/settings", Some(json!({ "currency": "JPY" })), &person).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, settings) =
        call(&app, "PUT", "/api/account/assist/settings", Some(json!({ "currency": null })), &person).await;
    assert_eq!(settings["currency"], Value::Null);

    let bad = json!({ "outputPricePerMillion": 1e9 });
    let (status, _) = call(&app, "PATCH", &format!("/api/admin/assist/providers/{id}"), Some(bad), &admin).await;
    assert_eq!(status, StatusCode::CONFLICT);
}
