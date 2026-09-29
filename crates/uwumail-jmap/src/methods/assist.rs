//! The AI assistant, `urn:uwumail:jmap:assist` (docs/jmap-assist.md): the person's providers and
//! choices, the features, labels and usage. The work is done by `uwumail_assist`; this turns JMAP
//! arguments into its calls and its answers into JMAP.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use uwumail_assist::{
    Assist, AssistError, Choice, ComposeArgs, Effective, EventsArgs, ProviderInput, ProviderView, SettingsPatch,
    SettingsView, SpamArgs, StreamEvent, SummarizeArgs, TodayUsage, Usage,
};
use uwumail_store::{Account, AssistLabel, StoreError};

use super::{Ctx, SetResponse, get_ids, if_in_state};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{dates, ids};

const SETTINGS_ID: &str = "singleton";
/// Emails `AssistLabel/apply` takes at once.
const MAX_APPLY: usize = 20;
const MAX_LOG: usize = 500;

pub fn provider_id(id: i64) -> String {
    format!("q{id}")
}

pub fn label_id(id: i64) -> String {
    format!("g{id}")
}

fn log_id(id: i64) -> String {
    format!("y{id}")
}

/// The assistant, when the server has one.
pub fn assist<'a>(ctx: &'a Ctx<'_>) -> MethodResult<&'a Assist> {
    ctx.jmap.assist.as_ref().ok_or_else(|| MethodError::new("unknownMethod", "the AI assistant is not set up here"))
}

pub fn method_error(err: AssistError) -> MethodError {
    match err {
        AssistError::Unavailable(description) => MethodError::new("assistUnavailable", description),
        AssistError::OverQuota(description) => MethodError::new("overQuota", description),
        AssistError::ProviderFailed { description, retry_after, .. } => {
            let mut error = MethodError::new("providerFailed", description);
            if let Some(secs) = retry_after {
                error.extra.insert("retryAfter".into(), json!(secs));
            }
            error
        }
        AssistError::Busy => {
            let mut error = MethodError::new("providerFailed", AssistError::Busy.to_string());
            error.extra.insert("retryAfter".into(), json!(5));
            error
        }
        AssistError::NotFound(what) => MethodError::new("notFound", format!("{what} not found")),
        AssistError::Forbidden(description) => MethodError::new("forbidden", description),
        AssistError::Invalid { description, .. } => MethodError::invalid_arguments(description),
        AssistError::Store(err) => err.into(),
    }
}

fn set_error(err: AssistError) -> SetError {
    match err {
        AssistError::Invalid { code: "tooManyProviders", description, .. } => SetError::new("overQuota", description),
        AssistError::Invalid { property, description, .. } => SetError::invalid_properties(&[property], description),
        AssistError::Forbidden(description) => SetError::new("forbidden", description),
        AssistError::NotFound(_) => SetError::not_found(),
        AssistError::Store(StoreError::Rule { code: "overQuota", message }) => SetError::new("overQuota", message),
        AssistError::Store(StoreError::Rule { message, .. }) => SetError::invalid_properties(&["name"], message),
        AssistError::Store(StoreError::NotFound(_)) => SetError::not_found(),
        other => SetError::new("serverFail", other.to_string()),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> MethodResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(MethodError::invalid_arguments(format!("{key} must be a string"))),
    }
}

fn arg_id(ctx: &Ctx<'_>, args: &Value, key: &str, prefix: char) -> MethodResult<Option<i64>> {
    match arg_str(args, key)? {
        None => Ok(None),
        Some(id) => ctx
            .parse_id(prefix, id)
            .map(Some)
            .ok_or_else(|| MethodError::new("notFound", format!("{key} {id} is not in this account"))),
    }
}

fn required_id(ctx: &Ctx<'_>, args: &Value, key: &str, prefix: char) -> MethodResult<i64> {
    arg_id(ctx, args, key, prefix)?.ok_or_else(|| MethodError::invalid_arguments(format!("{key} is required")))
}

fn usage_json(usage: &Usage) -> Value {
    json!({ "inputTokens": usage.input_tokens, "outputTokens": usage.output_tokens })
}

/// `providerId`, `providerName`, `model` and `usage` of every feature's answer.
fn with_source(mut out: Map<String, Value>, effective: &Effective, usage: &Usage) -> Value {
    out.insert("providerId".into(), json!(provider_id(effective.provider_id)));
    out.insert("providerName".into(), json!(effective.provider_name));
    out.insert("model".into(), json!(effective.model));
    out.insert("usage".into(), usage_json(usage));
    Value::Object(out)
}

pub fn provider_json(view: &ProviderView) -> Value {
    json!({
        "id": provider_id(view.id),
        "name": view.name,
        "kind": view.kind,
        "scope": view.scope,
        "baseUrl": view.base_url,
        "hasKey": view.has_key,
        "keyHint": view.key_hint,
        "model": view.model,
        "fastModel": view.fast_model,
        "features": view.features,
        "quota": view.quota,
        "experimental": view.experimental,
        "connected": view.connected,
    })
}

fn choice_json(choice: &Option<Choice>) -> Value {
    match choice {
        Some(choice) => json!({ "providerId": provider_id(choice.provider_id), "model": choice.model }),
        None => Value::Null,
    }
}

fn settings_json(view: &SettingsView) -> Value {
    let features: Map<String, Value> =
        view.features.iter().map(|(feature, choice)| (feature.clone(), choice_json(choice))).collect();
    let effective: Map<String, Value> = view
        .effective
        .iter()
        .map(|(feature, found)| {
            let value = match found {
                Some(found) => json!({
                    "providerId": provider_id(found.provider_id),
                    "providerName": found.provider_name,
                    "model": found.model,
                    "scope": found.scope,
                }),
                None => Value::Null,
            };
            (feature.clone(), value)
        })
        .collect();
    json!({
        "id": SETTINGS_ID,
        "default": choice_json(&view.default),
        "features": features,
        "autoLabels": view.auto_labels,
        "effective": effective,
    })
}

fn label_json(label: &AssistLabel) -> Value {
    json!({
        "id": label_id(label.id),
        "name": label.name,
        "description": label.description,
        "keyword": label.keyword,
        "color": label.color,
    })
}

fn today_json(today: &[TodayUsage]) -> Value {
    Value::Array(
        today
            .iter()
            .map(|row| {
                json!({
                    "providerId": provider_id(row.provider_id),
                    "providerName": row.provider_name,
                    "requests": row.requests,
                    "tokens": row.tokens,
                    "requestsPerDay": row.requests_per_day,
                    "tokensPerDay": row.tokens_per_day,
                })
            })
            .collect(),
    )
}

async fn state(ctx: &Ctx<'_>) -> MethodResult<String> {
    assist(ctx)?.state(&ctx.account).await.map_err(method_error)
}

/// A /get answer over a list the account has as a whole.
fn get_answer(ctx: &Ctx<'_>, args: &Value, state: String, all: Vec<(String, Value)>) -> MethodResult<Value> {
    let (list, not_found): (Vec<Value>, Vec<String>) = match get_ids(args)? {
        None => (all.into_iter().map(|(_, value)| value).collect(), Vec::new()),
        Some(wanted) => {
            let mut list = Vec::new();
            let mut missing = Vec::new();
            for id in wanted {
                match all.iter().find(|(known, _)| *known == id) {
                    Some((_, value)) => list.push(value.clone()),
                    None => missing.push(id),
                }
            }
            (list, missing)
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn provider_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let state = state(ctx).await?;
    let providers = assist.providers(&ctx.account).await.map_err(method_error)?;
    let all = providers.iter().map(|view| (provider_id(view.id), provider_json(view))).collect();
    get_answer(ctx, args, state, all)
}

/// The properties a person may set on their own provider.
fn provider_input(patch: &Value, create: bool) -> Result<ProviderInput, SetError> {
    let object = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "a provider is an object"))?;
    for key in object.keys() {
        let allowed = matches!(key.as_str(), "name" | "baseUrl" | "apiKey" | "model" | "fastModel")
            || (create && key == "kind")
            || matches!(
                key.as_str(),
                "id" | "scope" | "hasKey" | "keyHint" | "features" | "quota" | "experimental" | "connected"
            ) && !create;
        if !allowed {
            return Err(SetError::invalid_properties(&[key.as_str()], format!("{key} can't be set")));
        }
    }
    let settable: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "name" | "kind" | "baseUrl" | "apiKey" | "model" | "fastModel"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    serde_json::from_value(Value::Object(settable))
        .map_err(|err| SetError::invalid_properties(&["name"], format!("a property has the wrong type: {err}")))
}

pub async fn provider_set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let old_state = state(ctx).await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    let account = &ctx.account;
    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result = match provider_input(object, true) {
                Ok(input) => assist.create_personal_provider(account, input).await.map_err(set_error),
                Err(err) => Err(err),
            };
            match result {
                Ok(view) => {
                    response.created.insert(creation_id.clone(), provider_json(&view));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result = match (ctx.parse_id('q', id), provider_input(patch, false)) {
                (None, _) => Err(SetError::not_found()),
                (_, Err(err)) => Err(err),
                (Some(provider), Ok(input)) => {
                    assist.update_personal_provider(account, provider, input).await.map_err(set_error)
                }
            };
            match result {
                Ok(view) => {
                    let changed =
                        json!({ "hasKey": view.has_key, "keyHint": view.key_hint, "connected": view.connected });
                    response.updated.insert(id.clone(), changed);
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('q', id) {
                None => Err(SetError::not_found()),
                Some(provider) => assist.delete_personal_provider(account, provider).await.map_err(set_error),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }
    let new_state = state(ctx).await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

pub async fn provider_models(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let id = required_id(ctx, args, "providerId", 'q')?;
    let (models, model, fast) = assist.models(Some(&ctx.account), id).await.map_err(method_error)?;
    let models: Vec<Value> = models.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    Ok(json!({
        "accountId": ctx.account_id(),
        "providerId": provider_id(id),
        "models": models,
        "model": model,
        "fastModel": fast,
    }))
}

pub async fn chatgpt_login(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let id = required_id(ctx, args, "providerId", 'q')?;
    let (user_code, verification_uri, interval, expires_at) =
        assist.chatgpt_login(&ctx.account, id).await.map_err(method_error)?;
    Ok(json!({
        "accountId": ctx.account_id(),
        "providerId": provider_id(id),
        "userCode": user_code,
        "verificationUri": verification_uri,
        "interval": interval,
        "expiresAt": dates::format(expires_at),
    }))
}

pub async fn chatgpt_poll(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let id = required_id(ctx, args, "providerId", 'q')?;
    let (status, description) = assist.chatgpt_poll(&ctx.account, id).await.map_err(method_error)?;
    let mut out = json!({ "accountId": ctx.account_id(), "providerId": provider_id(id), "status": status });
    if let Some(description) = description {
        out["description"] = json!(description);
    }
    Ok(out)
}

pub async fn settings_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let view = assist.settings(&ctx.account).await.map_err(method_error)?;
    let state = state(ctx).await?;
    get_answer(ctx, args, state, vec![(SETTINGS_ID.to_owned(), settings_json(&view))])
}

fn parse_choice(ctx: &Ctx<'_>, value: &Value, property: &str) -> Result<Option<Choice>, SetError> {
    let bad = || SetError::invalid_properties(&[property], "a choice is {providerId, model} or null");
    match value {
        Value::Null => Ok(None),
        Value::Object(object) => {
            let provider = object.get("providerId").and_then(Value::as_str).ok_or_else(bad)?;
            let provider = ctx.parse_id('q', provider).ok_or_else(bad)?;
            let model = match object.get("model") {
                None | Some(Value::Null) => None,
                Some(Value::String(model)) => Some(model.clone()),
                Some(_) => return Err(bad()),
            };
            Ok(Some(Choice { provider_id: provider, model }))
        }
        _ => Err(bad()),
    }
}

/// A patch of `AssistSettings`, whole properties or `features/<feature>` paths.
fn settings_patch(ctx: &Ctx<'_>, patch: &Map<String, Value>) -> Result<SettingsPatch, SetError> {
    let mut out = SettingsPatch::default();
    let mut features: BTreeMap<String, Option<Choice>> = BTreeMap::new();
    for (key, value) in patch {
        match key.as_str() {
            "id" if value.as_str() == Some(SETTINGS_ID) => {}
            "effective" => {}
            "default" => out.default = Some(parse_choice(ctx, value, "default")?),
            "autoLabels" => {
                out.auto_labels = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| SetError::invalid_properties(&["autoLabels"], "autoLabels is true or false"))?,
                )
            }
            "features" => {
                let object = value
                    .as_object()
                    .ok_or_else(|| SetError::invalid_properties(&["features"], "features is an object"))?;
                // The whole map: features left out have no choice of their own any more.
                for feature in uwumail_assist::FEATURES {
                    features.insert(feature.to_owned(), None);
                }
                for (feature, choice) in object {
                    features.insert(feature.clone(), parse_choice(ctx, choice, "features")?);
                }
            }
            path if path.starts_with("features/") => {
                let feature = &path["features/".len()..];
                features.insert(feature.to_owned(), parse_choice(ctx, value, "features")?);
            }
            other => {
                return Err(SetError::invalid_properties(
                    &[other],
                    format!("{other} is not an AssistSettings property"),
                ));
            }
        }
    }
    if !features.is_empty() {
        out.features = Some(features);
    }
    Ok(out)
}

pub async fn settings_set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let old_state = state(ctx).await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for creation_id in create.keys() {
            response
                .not_created
                .insert(creation_id.clone(), SetError::new("singleton", "the settings exist once").to_json());
        }
    }
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result = async {
                if id != SETTINGS_ID {
                    return Err(SetError::not_found());
                }
                let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch is an object"))?;
                let patch = settings_patch(ctx, patch)?;
                assist.set_settings(&ctx.account, patch).await.map_err(set_error)
            }
            .await;
            match result {
                Ok(view) => {
                    response.updated.insert(id.clone(), json!({ "effective": settings_json(&view)["effective"] }));
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            response
                .not_destroyed
                .insert(id.to_owned(), SetError::new("singleton", "the settings can't be destroyed").to_json());
        }
    }
    let new_state = state(ctx).await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

fn compose_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<ComposeArgs> {
    let text = |key: &str| arg_str(args, key).map(|value| value.map(str::to_owned));
    Ok(ComposeArgs {
        mode: text("mode")?.ok_or_else(|| MethodError::invalid_arguments("mode is required"))?,
        instruction: text("instruction")?,
        preset: text("preset")?,
        target_language: text("targetLanguage")?,
        text: text("text")?,
        subject: text("subject")?,
        reply_to_email_id: arg_id(ctx, args, "replyToEmailId", 'e')?,
        want_subject: args.get("wantSubject").and_then(Value::as_bool).unwrap_or(false),
        language: text("language")?,
    })
}

fn summarize_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<SummarizeArgs> {
    Ok(SummarizeArgs {
        email_id: arg_id(ctx, args, "emailId", 'e')?,
        thread_id: arg_id(ctx, args, "threadId", 't')?,
        language: arg_str(args, "language")?.map(str::to_owned),
    })
}

/// `Assist/compose`, streamed to `events` when given.
pub async fn compose_with(
    assist: &Assist,
    account: &Account,
    account_id: String,
    args: ComposeArgs,
    events: Option<&mpsc::Sender<StreamEvent>>,
) -> MethodResult<Value> {
    let result = assist.compose(account, args, events).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(account_id));
    out.insert("text".into(), json!(result.text));
    out.insert("subject".into(), json!(result.subject));
    Ok(with_source(out, &result.effective, &result.usage))
}

/// `Assist/summarize`, streamed to `events` when given.
pub async fn summarize_with(
    assist: &Assist,
    account: &Account,
    account_id: String,
    args: SummarizeArgs,
    events: Option<&mpsc::Sender<StreamEvent>>,
) -> MethodResult<Value> {
    let (email, thread) = (args.email_id, args.thread_id);
    let result = assist.summarize(account, args, events).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(account_id));
    out.insert("emailId".into(), json!(email.map(ids::email)));
    out.insert("threadId".into(), json!(thread.map(ids::thread)));
    out.insert("summary".into(), json!(result.summary));
    Ok(with_source(out, &result.effective, &result.usage))
}

pub async fn compose(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    compose_with(assist, &ctx.account, ctx.account_id(), compose_args(ctx, args)?, None).await
}

pub async fn summarize(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    summarize_with(assist, &ctx.account, ctx.account_id(), summarize_args(ctx, args)?, None).await
}

/// The arguments of a streamed call, checked the same way as over `/jmap/api`.
pub enum StreamCall {
    Compose(ComposeArgs),
    Summarize(SummarizeArgs),
}

pub fn stream_call(ctx: &Ctx<'_>, method: &str, args: &Value) -> MethodResult<StreamCall> {
    ctx.check_account(args)?;
    match method {
        "Assist/compose" => Ok(StreamCall::Compose(compose_args(ctx, args)?)),
        "Assist/summarize" => Ok(StreamCall::Summarize(summarize_args(ctx, args)?)),
        _ => Err(MethodError::kind("unknownMethod")),
    }
}

pub async fn spam_check(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let email_id = required_id(ctx, args, "emailId", 'e')?;
    let language = arg_str(args, "language")?.map(str::to_owned);
    let result = assist.spam_check(&ctx.account, SpamArgs { email_id, language }).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(ctx.account_id()));
    out.insert("emailId".into(), json!(ids::email(email_id)));
    out.insert("verdict".into(), json!(result.verdict));
    out.insert("confidence".into(), json!(result.confidence));
    out.insert("reasons".into(), json!(result.reasons));
    out.insert("signals".into(), json!(result.signals));
    Ok(with_source(out, &result.effective, &result.usage))
}

pub async fn extract_events(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let email_id = required_id(ctx, args, "emailId", 'e')?;
    let include_images = args.get("includeImages").and_then(Value::as_bool).unwrap_or(false);
    let result =
        assist.extract_events(&ctx.account, EventsArgs { email_id, include_images }).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(ctx.account_id()));
    out.insert("emailId".into(), json!(ids::email(email_id)));
    out.insert("events".into(), json!(result.events));
    Ok(with_source(out, &result.effective, &result.usage))
}

pub async fn usage(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let days = match args.get("days") {
        None | Some(Value::Null) => 30,
        Some(value) => value
            .as_u64()
            .filter(|days| (1..=90).contains(days))
            .ok_or_else(|| MethodError::invalid_arguments("days is 1 to 90"))? as u32,
    };
    let (rows, today) = assist.usage(&ctx.account, days).await.map_err(method_error)?;
    let days: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "day": row.day,
                "providerId": provider_id(row.provider_id),
                "providerName": row.provider_name.clone().unwrap_or_default(),
                "feature": row.feature,
                "requests": row.requests,
                "inputTokens": row.input_tokens,
                "outputTokens": row.output_tokens,
            })
        })
        .collect();
    Ok(json!({ "accountId": ctx.account_id(), "days": days, "today": today_json(&today) }))
}

pub async fn label_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    assist(ctx)?;
    let state = state(ctx).await?;
    let labels = ctx.jmap.store.assist_labels(ctx.account.id).await?;
    let all = labels.iter().map(|label| (label_id(label.id), label_json(label))).collect();
    get_answer(ctx, args, state, all)
}

/// Name, description and color of a label, on top of `before`.
fn label_fields(patch: &Value, before: Option<&AssistLabel>) -> Result<(String, String, Option<String>), SetError> {
    let object = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "a label is an object"))?;
    let mut name = before.map(|b| b.name.clone());
    let mut description = before.map(|b| b.description.clone()).unwrap_or_default();
    let mut color = before.and_then(|b| b.color.clone());
    for (key, value) in object {
        match key.as_str() {
            "name" => {
                name = Some(
                    value
                        .as_str()
                        .ok_or_else(|| SetError::invalid_properties(&["name"], "name is a string"))?
                        .to_owned(),
                )
            }
            "description" => {
                description = match value {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    _ => return Err(SetError::invalid_properties(&["description"], "description is a string")),
                }
            }
            "color" => {
                color = match value {
                    Value::Null => None,
                    Value::String(text) => Some(text.to_ascii_lowercase()),
                    _ => return Err(SetError::invalid_properties(&["color"], "color is #rrggbb or null")),
                }
            }
            "id" if before.is_some() => {}
            "keyword" if before.is_some_and(|b| value.as_str() == Some(b.keyword.as_str())) => {}
            other => {
                return Err(SetError::invalid_properties(&[other], format!("{other} can't be set")));
            }
        }
    }
    let name = name.ok_or_else(|| SetError::invalid_properties(&["name"], "a label needs a name"))?;
    Ok((name, description, color))
}

fn label_store_error(err: StoreError) -> SetError {
    match err {
        StoreError::Rule { code: "overQuota", message } => SetError::new("overQuota", message),
        StoreError::Rule { message, .. } => SetError::invalid_properties(&["name"], message),
        StoreError::NotFound(_) => SetError::not_found(),
        other => other.into(),
    }
}

pub async fn label_set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let old_state = state(ctx).await?;
    if_in_state(args, &old_state)?;
    let store = &ctx.jmap.store;
    let account = &ctx.account;
    let mut response = SetResponse::default();
    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result = match label_fields(object, None) {
                Ok((name, description, color)) => {
                    store.create_assist_label(account.id, name, description, color).await.map_err(label_store_error)
                }
                Err(err) => Err(err),
            };
            match result {
                Ok(label) => {
                    response
                        .created
                        .insert(creation_id.clone(), json!({ "id": label_id(label.id), "keyword": label.keyword }));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        let labels = store.assist_labels(account.id).await?;
        for (id, patch) in update {
            let before = ctx.parse_id('g', id).and_then(|id| labels.iter().find(|label| label.id == id));
            let result = match before {
                None => Err(SetError::not_found()),
                Some(before) => match label_fields(patch, Some(before)) {
                    Ok((name, description, color)) => store
                        .update_assist_label(account.id, before.id, name, description, color)
                        .await
                        .map_err(label_store_error),
                    Err(err) => Err(err),
                },
            };
            match result {
                Ok(_) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('g', id) {
                None => Err(SetError::not_found()),
                Some(label) => assist.delete_label(account, label).await.map_err(set_error),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }
    let new_state = state(ctx).await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

fn email_ids(ctx: &Ctx<'_>, args: &Value, max: usize) -> MethodResult<Option<(Vec<i64>, Vec<String>)>> {
    match args.get("emailIds") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            if items.len() > max {
                return Err(MethodError::kind("requestTooLarge"));
            }
            let mut found = Vec::new();
            let mut unknown = Vec::new();
            for item in items {
                let id = item.as_str().ok_or_else(|| MethodError::invalid_arguments("emailIds are strings"))?;
                match ctx.parse_id('e', id) {
                    Some(parsed) => found.push(parsed),
                    None => unknown.push(id.to_owned()),
                }
            }
            Ok(Some((found, unknown)))
        }
        Some(_) => Err(MethodError::invalid_arguments("emailIds is an array or null")),
    }
}

pub async fn label_log(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    assist(ctx)?;
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 100,
        Some(value) => value
            .as_u64()
            .filter(|limit| (1..=MAX_LOG as u64).contains(limit))
            .ok_or_else(|| MethodError::invalid_arguments("limit is 1 to 500"))? as usize,
    };
    let emails = email_ids(ctx, args, MAX_LOG)?.map(|(found, _)| found);
    let entries = ctx.jmap.store.label_log(ctx.account.id, emails, limit).await?;
    let list: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "id": log_id(entry.id),
                "emailId": ids::email(entry.email_id),
                "labelId": label_id(entry.label_id),
                "name": entry.name,
                "keyword": entry.keyword,
                "reason": entry.reason,
                "providerName": entry.provider,
                "model": entry.model,
                "createdAt": dates::format(entry.created_at),
                "undone": entry.undone,
            })
        })
        .collect();
    Ok(json!({ "accountId": ctx.account_id(), "list": list }))
}

pub async fn label_undo(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let wanted = get_ids(args)?.ok_or_else(|| MethodError::invalid_arguments("ids is required"))?;
    let mut undone = Vec::new();
    let mut not_found = Vec::new();
    for id in wanted {
        let found = match ctx.parse_id('y', &id) {
            Some(log) => assist.undo_label(&ctx.account, log).await.map_err(method_error)?,
            None => false,
        };
        if found { undone.push(id) } else { not_found.push(id) }
    }
    Ok(json!({ "accountId": ctx.account_id(), "undone": undone, "notFound": not_found }))
}

pub async fn label_apply(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let (emails, mut not_found) =
        email_ids(ctx, args, MAX_APPLY)?.ok_or_else(|| MethodError::invalid_arguments("emailIds is required"))?;
    let mut labeled = Map::new();
    for email in emails {
        match assist.label_email(&ctx.account, email).await {
            Ok(picks) => {
                let labels: Vec<String> = picks.iter().map(|pick| label_id(pick.label.id)).collect();
                labeled.insert(ids::email(email), json!(labels));
            }
            Err(AssistError::NotFound(_)) => not_found.push(ids::email(email)),
            Err(err) => return Err(method_error(err)),
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "labeled": labeled, "notFound": not_found }))
}
