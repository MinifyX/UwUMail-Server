//! Who may use what: the admin's policy, access lists, quotas, sealed keys and the addresses a
//! provider may point to.

use serde_json::{Value, json};
use uwumail_assist::{Assist, AssistError, ComposeArgs, ProviderInput, SettingsPatch, SpamArgs};
use uwumail_smtp::egress::Egress;

use crate::common::{INVOICE, Reply, account, chat, rig};

const KEY: &str = "sk-test-abcdefgh1234";

fn input(value: Value) -> ProviderInput {
    serde_json::from_value(value).unwrap()
}

fn code(err: AssistError) -> &'static str {
    match err {
        AssistError::Invalid { code, .. } => code,
        AssistError::Forbidden(_) => "forbidden",
        AssistError::Unavailable(_) => "unavailable",
        AssistError::OverQuota(_) => "overQuota",
        AssistError::NotFound(_) => "notFound",
        AssistError::ProviderFailed { .. } => "providerFailed",
        other => panic!("{other:?}"),
    }
}

fn write() -> ComposeArgs {
    ComposeArgs { mode: "write".into(), instruction: Some("Ein kurzer Gruß".into()), ..ComposeArgs::default() }
}

#[tokio::test]
async fn daily_limits_stop_before_the_provider_is_asked() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({ "requestsPerDay": 2 })).await;
    assert!(rig.assist.compose(&rig.mia, write(), None).await.is_ok());
    assert!(rig.assist.compose(&rig.mia, write(), None).await.is_ok());
    assert_eq!(code(rig.assist.compose(&rig.mia, write(), None).await.unwrap_err()), "overQuota");
    assert_eq!(rig.fake.seen().len(), 2);
    let today = rig.assist.today(&rig.mia).await.unwrap();
    assert_eq!((today[0].requests, today[0].tokens), (2, 300));

    // A token limit counts what the answers used.
    let rig = crate::common::rig().await;
    rig.server_provider("openaiCompatible", json!({ "tokensPerDay": 100 })).await;
    assert!(rig.assist.compose(&rig.mia, write(), None).await.is_ok());
    assert_eq!(code(rig.assist.compose(&rig.mia, write(), None).await.unwrap_err()), "overQuota");
    // Someone else still has their own allowance.
    let leni = account(&rig.store, "leni@example.org").await;
    assert!(rig.assist.compose(&leni, write(), None).await.is_ok());
}

#[tokio::test]
async fn keys_are_sealed_at_rest_and_never_shown() {
    let rig = rig().await;
    let id = rig.server_provider("openaiCompatible", json!({})).await;
    rig.policy(|policy| policy.allow_personal = true).await;
    let own = rig
        .assist
        .create_personal_provider(
            &rig.mia,
            input(json!({ "name": "Meins", "kind": "openaiCompatible", "baseUrl": rig.fake.base(), "apiKey": "sk-own-zyxw9876" })),
        )
        .await
        .unwrap();
    assert_eq!(own.key_hint.as_deref(), Some("…9876"));
    let admin = rig.assist.admin_providers().await.unwrap();
    assert_eq!(admin[0].key_hint.as_deref(), Some("…1234"));
    assert!(admin[0].has_key);
    let persons = rig.assist.providers(&rig.mia).await.unwrap();
    let server = persons.iter().find(|provider| provider.id == id).unwrap();
    assert_eq!(server.key_hint, None, "the end of the admin's key is the admin's (AI-07)");
    let shown = serde_json::to_string(&(admin, persons)).unwrap();
    assert!(!shown.contains(KEY) && !shown.contains("sk-own"), "{shown}");

    // Nowhere in the database files, not even in the write-ahead log.
    let mut files = 0;
    for entry in std::fs::read_dir(rig.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().starts_with("uwumail.db") {
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(KEY) && !text.contains("sk-own-zyxw9876"), "{path:?} holds a key");
            files += 1;
        }
    }
    assert!(files >= 1);
    let db = rusqlite::Connection::open(rig.dir.path().join("uwumail.db")).unwrap();
    let sealed: Vec<u8> =
        db.query_row("SELECT secret FROM assist_providers WHERE id = ?1", [id], |row| row.get(0)).unwrap();
    assert!(!sealed.is_empty());

    // An update without a key keeps it; the provider still gets it.
    rig.assist.update_server_provider(id, input(json!({ "name": "Umbenannt" }))).await.unwrap();
    rig.assist.compose(&rig.mia, write(), None).await.unwrap();
    assert_eq!(rig.fake.seen()[0].headers["authorization"], format!("Bearer {KEY}"));
}

#[tokio::test]
async fn own_providers_stay_on_the_internet_unless_the_admin_says_otherwise() {
    let rig = rig().await;
    // The real thing, not the test's permissive one.
    let assist = Assist::new(rig.store.clone(), Egress::direct(), "mx.example.org");
    let own = |base: String| {
        let assist = assist.clone();
        let mia = rig.mia.clone();
        async move {
            assist
                .create_personal_provider(
                    &mia,
                    input(json!({ "name": "Meins", "kind": "openaiCompatible", "baseUrl": base, "apiKey": "sk-own-1", "model": "m" })),
                )
                .await
        }
    };
    assert_eq!(code(own("https://api.example.com/v1".into()).await.unwrap_err()), "forbidden");
    rig.policy(|policy| policy.allow_personal = true).await;
    assert!(own("https://api.example.com/v1".into()).await.is_ok());
    assert_eq!(code(own("http://api.example.com/v1".into()).await.unwrap_err()), "plainHttpPublic");
    assert_eq!(code(own("http://192.168.1.5:11434/v1".into()).await.unwrap_err()), "privateAddress");
    assert_eq!(code(own(rig.fake.base()).await.unwrap_err()), "privateAddress");
    assert_eq!(code(own("http://ollama.local:11434/v1".into()).await.unwrap_err()), "privateAddress");

    rig.policy(|policy| policy.allow_personal_private = true).await;
    assert!(own("http://192.168.1.5:11434/v1".into()).await.is_ok());
    // This machine is not the local network.
    assert_eq!(code(own(rig.fake.base()).await.unwrap_err()), "privateAddress");
    assert_eq!(code(own("http://[::1]:11434/v1".into()).await.unwrap_err()), "privateAddress");
    // A name is let through here and checked where it resolves: localhost is still refused.
    let sneaky = own(format!("http://localhost:{}/v1", rig.fake.addr.port())).await.unwrap();
    let patch = SettingsPatch {
        default: Some(Some(serde_json::from_value(json!({ "providerId": sneaky.id })).unwrap())),
        ..SettingsPatch::default()
    };
    assist.set_settings(&rig.mia, patch).await.unwrap();
    assert_eq!(code(assist.compose(&rig.mia, write(), None).await.unwrap_err()), "providerFailed");
    assert!(rig.fake.seen().is_empty(), "the fake on this machine was never reached");

    // The admin's own providers may point anywhere, this machine included.
    let admin = assist
        .create_server_provider(input(json!({
            "name": "Ollama", "kind": "ollama", "baseUrl": rig.fake.base(), "model": "llama3.2"
        })))
        .await
        .unwrap();
    let patch = SettingsPatch {
        default: Some(Some(serde_json::from_value(json!({ "providerId": admin.id })).unwrap())),
        ..SettingsPatch::default()
    };
    assist.set_settings(&rig.mia, patch).await.unwrap();
    assist.compose(&rig.mia, write(), None).await.unwrap();
    assert_eq!(rig.fake.seen().len(), 1);
}

#[tokio::test]
async fn the_policy_decides_who_gets_which_feature() {
    let rig = rig().await;
    rig.store.create_domain("example.net").await.unwrap();
    let tom = account(&rig.store, "tom@example.net").await;
    let email = rig.deliver(&rig.mia, INVOICE).await;
    let id = rig
        .server_provider(
            "openaiCompatible",
            json!({ "access": "domains", "domains": ["Example.org"], "features": ["compose", "spamCheck"] }),
        )
        .await;
    let capability = rig.assist.capability(&rig.mia).await.unwrap();
    assert!(capability.features.compose && capability.features.spam_check);
    assert!(!capability.features.summarize, "the provider does not offer it");
    assert!(!rig.assist.capability(&tom).await.unwrap().features.compose, "not tom's domain");
    assert_eq!(code(rig.assist.compose(&tom, write(), None).await.unwrap_err()), "unavailable");
    assert!(rig.assist.providers(&tom).await.unwrap().is_empty());

    let summarize = uwumail_assist::SummarizeArgs { email_id: Some(email), ..Default::default() };
    assert_eq!(code(rig.assist.summarize(&rig.mia, summarize, None).await.unwrap_err()), "unavailable");

    // Switched off on the server: gone for everyone.
    rig.policy(|policy| policy.features.spam_check = false).await;
    let spam = SpamArgs { email_id: email, language: None };
    assert_eq!(code(rig.assist.spam_check(&rig.mia, spam).await.unwrap_err()), "unavailable");

    // By person instead of domain.
    rig.assist
        .update_server_provider(id, input(json!({ "access": "people", "people": ["tom@example.net"] })))
        .await
        .unwrap();
    assert!(rig.assist.compose(&tom, write(), None).await.is_ok());
    assert_eq!(code(rig.assist.compose(&rig.mia, write(), None).await.unwrap_err()), "unavailable");
    assert_eq!(
        code(
            rig.assist
                .update_server_provider(id, input(json!({ "access": "people", "people": [] })))
                .await
                .unwrap_err()
        ),
        "badAccess"
    );
    // A disabled provider is nobody's.
    rig.assist.update_server_provider(id, input(json!({ "enabled": false }))).await.unwrap();
    assert_eq!(code(rig.assist.compose(&tom, write(), None).await.unwrap_err()), "unavailable");
    assert_eq!(rig.fake.seen().len(), 1);
}

#[tokio::test]
async fn choices_pick_the_provider_and_model() {
    let rig = rig().await;
    let first = rig.server_provider("openaiCompatible", json!({ "name": "Erster" })).await;
    let second = rig.server_provider("openaiCompatible", json!({ "name": "Zweiter" })).await;
    let settings = rig.assist.settings(&rig.mia).await.unwrap();
    let effective = settings.effective["compose"].clone().unwrap();
    assert_eq!((effective.provider_id, effective.model.as_str()), (first, "big-model"));

    let patch: SettingsPatch = serde_json::from_value(json!({
        "default": { "providerId": second },
        "features": { "spamCheck": { "providerId": first, "model": "tiny-model" } }
    }))
    .unwrap();
    let settings = rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    assert_eq!(settings.effective["compose"].as_ref().unwrap().provider_id, second);
    let spam = settings.effective["spamCheck"].clone().unwrap();
    assert_eq!((spam.provider_id, spam.model.as_str()), (first, "tiny-model"));

    let email = rig.deliver(&rig.mia, INVOICE).await;
    rig.fake.push(Reply::Json(200, chat(r#"{"verdict":"legitimate","confidence":0.9,"reasons":[]}"#), vec![]));
    let result = rig.assist.spam_check(&rig.mia, SpamArgs { email_id: email, language: None }).await.unwrap();
    assert_eq!(result.effective.provider_name, "Erster");
    assert_eq!(rig.fake.seen()[0].body["model"], "tiny-model");

    // A choice of a provider that is gone falls back to what is there.
    rig.assist.delete_server_provider(second).await.unwrap();
    let settings = rig.assist.settings(&rig.mia).await.unwrap();
    assert_eq!(settings.effective["compose"].as_ref().unwrap().provider_id, first);

    // Nobody chooses someone else's provider.
    let patch: SettingsPatch = serde_json::from_value(json!({ "default": { "providerId": 999 } })).unwrap();
    assert!(rig.assist.set_settings(&rig.mia, patch).await.is_err());
}

/// AI-01 of the 0.18.0 audit: checking and counting a request are one step, so requests side by
/// side cannot share the last one left of a day.
#[tokio::test]
async fn requests_side_by_side_do_not_share_the_last_one() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({ "requestsPerDay": 1 })).await;
    let (a, b, c) = tokio::join!(
        rig.assist.compose(&rig.mia, write(), None),
        rig.assist.compose(&rig.mia, write(), None),
        rig.assist.compose(&rig.mia, write(), None),
    );
    let ok = [a, b, c].into_iter().filter(Result::is_ok).count();
    assert_eq!(ok, 1);
    assert_eq!(rig.fake.seen().len(), 1);
}

/// AI-01: a streamed answer whose listener goes away is counted all the same.
#[tokio::test]
async fn a_stream_that_is_left_is_still_counted() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({ "tokensPerDay": 1_000_000 })).await;
    let long: Vec<String> = (0..200).map(|n| format!("Satz {n} eines langen Entwurfs. ")).collect();
    let pieces: Vec<&str> = long.iter().map(String::as_str).collect();
    rig.fake.push(Reply::Stream(crate::common::chat_stream(&pieces), 64));
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let work = rig.assist.compose(&rig.mia, write(), Some(&tx));
    let listener = async {
        // The first piece, then the reader goes away.
        rx.recv().await;
        drop(rx);
    };
    let (result, ()) = tokio::join!(work, listener);
    assert!(result.is_err(), "nobody listened to the end");
    let today = rig.assist.today(&rig.mia).await.unwrap();
    assert_eq!(today[0].requests, 1);
    assert!(today[0].tokens > 0, "{:?}", today[0].tokens);
}
