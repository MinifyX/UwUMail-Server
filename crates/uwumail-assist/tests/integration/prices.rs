//! What models cost: the price lists fetched from stand-ins for LiteLLM, OpenRouter and the ECB,
//! kept when a fetch fails, and the costs of estimates and of what was used.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::get;
use serde_json::json;
use uwumail_assist::{
    Assist, ComposeArgs, EstimateArgs, PriceSource, PriceSources, ProviderInput, SummarizeArgs, chatgpt,
};

use crate::common::{INVOICE, Reply, chat, rig};

const LITELLM: &str = r#"{
    "big-model": { "input_cost_per_token": 0.000002, "output_cost_per_token": 0.00001, "mode": "chat" },
    "small-model-2026-01-15": { "input_cost_per_token": 0.0000001, "output_cost_per_token": 0.0000004 }
}"#;
const ECB: &str = "<?xml version='1.0' encoding='UTF-8'?><gesmes:Envelope><Cube><Cube time='2026-09-29'>\
    <Cube currency='USD' rate='1.25'/><Cube currency='JPY' rate='160'/></Cube></Cube></gesmes:Envelope>";
const OPENROUTER: &str =
    r#"{ "data": [{ "id": "openai/gpt-5-mini", "pricing": { "prompt": "0.000001", "completion": "0.000004" } }] }"#;

/// Stand-ins for the three lists on 127.0.0.1:0; `down` makes them answer 503.
struct Lists {
    sources: PriceSources,
    down: Arc<AtomicBool>,
    hits: Arc<Mutex<Vec<String>>>,
}

async fn lists() -> Lists {
    let down = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(Mutex::new(Vec::new()));
    let serve = |body: &'static str, down: Arc<AtomicBool>, hits: Arc<Mutex<Vec<String>>>, name: &'static str| {
        get(move || async move {
            hits.lock().unwrap().push(name.to_owned());
            if down.load(Ordering::SeqCst) { Err(axum::http::StatusCode::SERVICE_UNAVAILABLE) } else { Ok(body) }
        })
    };
    let app = Router::new()
        .route("/litellm.json", serve(LITELLM, down.clone(), hits.clone(), "litellm"))
        .route("/eurofxref-daily.xml", serve(ECB, down.clone(), hits.clone(), "ecb"))
        .route("/api/v1/models", serve(OPENROUTER, down.clone(), hits.clone(), "openrouter"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let sources = PriceSources {
        litellm: format!("{base}/litellm.json"),
        ecb: format!("{base}/eurofxref-daily.xml"),
        openrouter: format!("{base}/api/v1/models"),
    };
    Lists { sources, down, hits }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[tokio::test]
async fn price_lists_are_fetched_kept_and_kept_when_a_fetch_fails() {
    let rig = rig().await;
    let lists = lists().await;
    let assist = Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default())
        .with_price_sources(lists.sources.clone());
    assert!(assist.refresh_prices().await.unwrap());
    assert_eq!(*lists.hits.lock().unwrap(), ["litellm", "ecb"], "OpenRouter only once someone uses it");
    let prices = assist.prices().await;
    assert_eq!(prices.table.models.len(), 2);
    assert_eq!(prices.table.rates_day.as_deref(), Some("2026-09-29"));

    // Another assistant on the same database starts from what was kept.
    let again = Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default());
    assert_eq!(again.prices().await.table, prices.table);

    rig.server_provider("openrouter", json!({ "model": "openai/gpt-5-mini" })).await;
    lists.down.store(true, Ordering::SeqCst);
    assert!(!assist.refresh_prices().await.unwrap(), "nothing came");
    let kept = assist.prices().await;
    assert_eq!((kept.table.models.len(), kept.table.rates.len()), (2, 2), "the last good copy stays");
    assert!(kept.table.openrouter.is_empty());

    lists.down.store(false, Ordering::SeqCst);
    assert!(assist.refresh_prices().await.unwrap());
    let providers = assist.admin_providers().await.unwrap();
    let price = providers[0].price.unwrap();
    assert!(close(price.input_per_million, 1.0) && close(price.output_per_million, 4.0), "{price:?}");
    assert_eq!(price.source, PriceSource::Auto);
}

#[tokio::test]
async fn costs_are_kept_with_the_usage_and_shown_as_the_admin_decides() {
    let rig = rig().await;
    let lists = lists().await;
    let assist = Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default())
        .with_price_sources(lists.sources.clone());
    assist.refresh_prices().await.unwrap();
    let server = rig.server_provider("openaiCompatible", json!({ "requestsPerDay": 100 })).await;
    let email = rig.deliver(&rig.mia, INVOICE).await;

    // The admin sees the price; the person does not, until the admin says so.
    let admin = assist.admin_providers().await.unwrap();
    assert!(!admin[0].show_cost_to_users);
    let price = admin[0].price.unwrap();
    assert!(close(price.input_per_million, 2.0) && close(price.output_per_million, 10.0), "big-model: {price:?}");
    assert_eq!(assist.providers(&rig.mia).await.unwrap()[0].price, None);
    let summarize = || EstimateArgs::Summarize(SummarizeArgs { email_id: Some(email), ..Default::default() });
    let hidden = assist.estimate(&rig.mia, summarize()).await.unwrap();
    assert_eq!(hidden.cost_usd, None);

    // The fake answers 120 tokens in, 30 out, with small-model (found without its date).
    rig.fake.push(Reply::Json(200, chat("Eine Rechnung."), vec![]));
    assist.summarize(&rig.mia, SummarizeArgs { email_id: Some(email), ..Default::default() }, None).await.unwrap();
    let stored = rig.store.assist_used_today(rig.mia.id, server).await.unwrap().cost_usd.unwrap();
    assert!(close(stored, (120.0 * 0.1 + 30.0 * 0.4) / 1e6), "{stored}");
    let (rows, today) = assist.usage(&rig.mia, 1).await.unwrap();
    assert_eq!((rows[0].cost_usd, today[0].cost_usd), (None, None), "hidden from the person");
    let admin_rows = rig.store.assist_usage(None, uwumail_store::utc_day(0)).await.unwrap();
    assert!(admin_rows[0].cost_usd.is_some(), "the admin sees it");

    let show: ProviderInput = serde_json::from_value(json!({ "showCostToUsers": true })).unwrap();
    assert!(assist.update_server_provider(server, show).await.unwrap().show_cost_to_users);
    let (rows, today) = assist.usage(&rig.mia, 1).await.unwrap();
    assert_eq!((rows[0].cost_usd, today[0].cost_usd), (Some(stored), Some(stored)));
    let shown = assist.estimate(&rig.mia, summarize()).await.unwrap();
    let expected = (shown.input_tokens as f64 * 0.1 + shown.output_tokens as f64 * 0.4) / 1e6;
    assert!(close(shown.cost_usd.unwrap(), expected));
    let prices = assist.prices().await;
    let euro = prices.convert(shown.cost_usd.unwrap(), "EUR").unwrap();
    assert!(close(euro.amount, expected / 1.25) && euro.currency == "EUR");

    // A price set by hand comes first; a bad one is refused.
    let manual: ProviderInput =
        serde_json::from_value(json!({ "inputPricePerMillion": 1.0, "outputPricePerMillion": 2.0 })).unwrap();
    let view = assist.update_server_provider(server, manual).await.unwrap();
    assert_eq!(view.price.unwrap().source, PriceSource::Manual);
    let manual = assist.estimate(&rig.mia, summarize()).await.unwrap();
    assert!(close(manual.cost_usd.unwrap(), (manual.input_tokens as f64 + manual.output_tokens as f64 * 2.0) / 1e6));
    let bad: ProviderInput = serde_json::from_value(json!({ "inputPricePerMillion": -1.0 })).unwrap();
    assert!(assist.update_server_provider(server, bad).await.is_err());
}

#[tokio::test]
async fn own_providers_always_show_their_cost_and_ollama_is_free() {
    let rig = rig().await;
    let lists = lists().await;
    let assist = Assist::for_tests(rig.store.clone(), "mx.example.org", chatgpt::Endpoints::default())
        .with_price_sources(lists.sources.clone());
    assist.refresh_prices().await.unwrap();
    rig.policy(|policy| policy.allow_personal = true).await;
    let own = |kind: &str| -> ProviderInput {
        serde_json::from_value(json!({
            "name": kind, "kind": kind, "baseUrl": rig.fake.base(), "apiKey": "sk-test-abcdefgh1234",
            "model": "big-model", "fastModel": "small-model", "showCostToUsers": true
        }))
        .unwrap()
    };
    let view = assist.create_personal_provider(&rig.mia, own("openaiCompatible")).await.unwrap();
    assert_eq!(view.price.unwrap().source, PriceSource::Auto, "their own, shown");
    let record = rig.store.assist_provider(view.id).await.unwrap().unwrap();
    assert!(!record.show_cost, "the switch is the admin's");

    let args = ComposeArgs { mode: "write".into(), instruction: Some("Sag zu".into()), ..Default::default() };
    let estimate = assist.estimate(&rig.mia, EstimateArgs::Compose(args)).await.unwrap();
    let expected = (estimate.input_tokens as f64 * 2.0 + estimate.output_tokens as f64 * 10.0) / 1e6;
    assert!(close(estimate.cost_usd.unwrap(), expected));
    assert!(estimate.requests_left_today.is_none());

    let ollama = assist.create_personal_provider(&rig.mia, own("ollama")).await.unwrap();
    let price = ollama.price.unwrap();
    assert_eq!((price.source, price.input_per_million, price.output_per_million), (PriceSource::Free, 0.0, 0.0));
}
