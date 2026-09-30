//! The AI assistant, `urn:uwumail:jmap:assist` (docs/jmap-assist.md): the person's providers and
//! choices, the features, labels and usage. The work is done by `uwumail_assist`; this turns JMAP
//! arguments into its calls and its answers into JMAP.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use uwumail_assist::{
    Assist, AssistError, Choice, ComposeArgs, Effective, EstimateArgs, EstimateCost, EventsArgs, ForeignMail,
    ProviderInput, ProviderView, SettingsPatch, SettingsView, SpamArgs, StreamEvent, SuggestArgs, SummarizeArgs,
    TodayUsage, Usage,
};
use uwumail_labels::{Detector, Rules};
use uwumail_store::{Account, AssistLabel, AssistLabelWrite, LabelCounts, StoreError};

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
    json!({
        "inputTokens": usage.input_tokens,
        "outputTokens": usage.output_tokens,
        "reasoningTokens": usage.reasoning_tokens,
    })
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
        "inputPricePerMillion": view.input_price_per_million,
        "outputPricePerMillion": view.output_price_per_million,
        "pricePerRequest": view.price_per_request,
        "price": view.price,
    })
}

/// `currency` of `Assist/estimate` and `Assist/usage`: ISO 4217, EUR when not given.
fn currency(args: &Value) -> MethodResult<String> {
    match arg_str(args, "currency")? {
        None => Ok("EUR".to_owned()),
        Some(code) if uwumail_assist::prices::is_currency(code) => Ok(code.to_owned()),
        Some(_) => Err(MethodError::invalid_arguments("currency is an ISO 4217 code like EUR")),
    }
}

/// `{ amount, currency, usd }` of a cost in US dollars, or null.
fn cost_json(prices: &uwumail_assist::Prices, usd: Option<f64>, currency: &str) -> Value {
    json!(usd.and_then(|usd| prices.convert(usd, currency)))
}

/// The `cost` of `Assist/estimate`: `{ amount, currency, usd, max: { amount, usd }, parts: { input,
/// output, reasoning, images, requests, other } }`, the parts in `currency`; null without a rate.
fn estimate_cost_json(prices: &uwumail_assist::Prices, cost: Option<&EstimateCost>, currency: &str) -> Value {
    let Some(cost) = cost else { return Value::Null };
    let (Some(total), Some(max), Some(unit)) =
        (prices.convert(cost.usd, currency), prices.convert(cost.max_usd, currency), prices.convert(1.0, currency))
    else {
        return Value::Null;
    };
    let parts = cost.parts.scaled(unit.amount);
    json!({
        "amount": total.amount,
        "currency": total.currency,
        "usd": total.usd,
        "max": { "amount": max.amount, "usd": max.usd },
        "parts": {
            "input": parts.input,
            "output": parts.output,
            "reasoning": parts.reasoning,
            "images": parts.images,
            "requests": parts.requests,
            "other": parts.other,
        },
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
        "nonAiLabels": view.non_ai_labels,
        "effective": effective,
    })
}

fn label_json(label: &AssistLabel, counts: LabelCounts) -> Value {
    json!({
        "id": label_id(label.id),
        "name": label.name,
        "description": label.description,
        "keyword": label.keyword,
        "color": label.color,
        "rules": label.rules,
        "detector": label.detector,
        "learnSenders": label.learn_senders,
        "classifier": label.classifier,
        "totalEmails": counts.total,
        "unreadEmails": counts.unread,
        "examples": counts.examples,
    })
}

fn today_json(today: &[TodayUsage], prices: &uwumail_assist::Prices, currency: &str) -> Value {
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
                    "cost": cost_json(prices, row.cost_usd, currency),
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
        let allowed = matches!(
            key.as_str(),
            "name"
                | "baseUrl"
                | "apiKey"
                | "model"
                | "fastModel"
                | "inputPricePerMillion"
                | "outputPricePerMillion"
                | "pricePerRequest"
        ) || (create && key == "kind")
            || matches!(
                key.as_str(),
                "id" | "scope" | "hasKey" | "keyHint" | "features" | "quota" | "experimental" | "connected" | "price"
            ) && !create;
        if !allowed {
            return Err(SetError::invalid_properties(&[key.as_str()], format!("{key} can't be set")));
        }
    }
    let settable: Map<String, Value> = object
        .iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "name"
                    | "kind"
                    | "baseUrl"
                    | "apiKey"
                    | "model"
                    | "fastModel"
                    | "inputPricePerMillion"
                    | "outputPricePerMillion"
                    | "pricePerRequest"
            )
        })
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
                    let changed = json!({
                        "hasKey": view.has_key, "keyHint": view.key_hint, "connected": view.connected, "price": view.price
                    });
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
            "nonAiLabels" => {
                out.non_ai_labels =
                    Some(value.as_bool().ok_or_else(|| {
                        SetError::invalid_properties(&["nonAiLabels"], "nonAiLabels is true or false")
                    })?)
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

/// `foreignMails`, `count` of them, when given: then `instead` (the mail ids the call takes
/// otherwise) must not be.
fn foreign_arg(
    args: &Value,
    count: std::ops::RangeInclusive<usize>,
    instead: &[&str],
) -> MethodResult<Vec<ForeignMail>> {
    let value = match args.get("foreignMails") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(value) => value,
    };
    if let Some(key) = instead.iter().find(|key| args.get(**key).is_some_and(|v| !v.is_null())) {
        return Err(MethodError::invalid_arguments(format!("give either {key} or foreignMails")));
    }
    uwumail_assist::foreign_mails(value, count).map_err(method_error)
}

fn compose_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<ComposeArgs> {
    let text = |key: &str| arg_str(args, key).map(|value| value.map(str::to_owned));
    let foreign_mails = foreign_arg(args, 1..=1, &["replyToEmailId"])?;
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
        foreign_mails,
    })
}

fn summarize_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<SummarizeArgs> {
    let foreign_mails = foreign_arg(args, 1..=uwumail_assist::foreign::MAX_FOREIGN_MAILS, &["emailId", "threadId"])?;
    Ok(SummarizeArgs {
        email_id: arg_id(ctx, args, "emailId", 'e')?,
        thread_id: arg_id(ctx, args, "threadId", 't')?,
        language: arg_str(args, "language")?.map(str::to_owned),
        foreign_mails,
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

/// The `emailId` of a call about one mail, or its one foreign mail (then the id is 0).
fn one_mail(ctx: &Ctx<'_>, args: &Value) -> MethodResult<(i64, Vec<ForeignMail>)> {
    let foreign_mails = foreign_arg(args, 1..=1, &["emailId"])?;
    let email_id = if foreign_mails.is_empty() { required_id(ctx, args, "emailId", 'e')? } else { 0 };
    Ok((email_id, foreign_mails))
}

/// `emailId` of an answer: `null` for a foreign mail.
fn answered_email(email_id: i64, foreign: bool) -> Value {
    if foreign { Value::Null } else { json!(ids::email(email_id)) }
}

fn spam_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<SpamArgs> {
    let (email_id, foreign_mails) = one_mail(ctx, args)?;
    let language = arg_str(args, "language")?.map(str::to_owned);
    Ok(SpamArgs { email_id, language, foreign_mails })
}

fn events_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<EventsArgs> {
    let (email_id, foreign_mails) = one_mail(ctx, args)?;
    let include_images = args.get("includeImages").and_then(Value::as_bool).unwrap_or(false);
    Ok(EventsArgs { email_id, include_images, foreign_mails })
}

fn suggest_args(ctx: &Ctx<'_>, args: &Value) -> MethodResult<SuggestArgs> {
    let (email_id, foreign_mails) = one_mail(ctx, args)?;
    let foreign_labels = match args.get("foreignLabels") {
        None | Some(Value::Null) => Vec::new(),
        Some(_) if foreign_mails.is_empty() => {
            return Err(MethodError::invalid_arguments("foreignLabels go with foreignMails"));
        }
        Some(value) => uwumail_assist::foreign_labels(value).map_err(method_error)?,
    };
    let suggest_new = match args.get("suggestNew") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(on)) => *on,
        Some(_) => return Err(MethodError::invalid_arguments("suggestNew is true or false")),
    };
    let language = arg_str(args, "language")?.map(str::to_owned);
    Ok(SuggestArgs { email_id, suggest_new, language, foreign_mails, foreign_labels })
}

pub async fn spam_check(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let args = spam_args(ctx, args)?;
    let email_id = answered_email(args.email_id, !args.foreign_mails.is_empty());
    let result = assist.spam_check(&ctx.account, args).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(ctx.account_id()));
    out.insert("emailId".into(), email_id);
    out.insert("verdict".into(), json!(result.verdict));
    out.insert("confidence".into(), json!(result.confidence));
    out.insert("reasons".into(), json!(result.reasons));
    out.insert("signals".into(), json!(result.signals));
    Ok(with_source(out, &result.effective, &result.usage))
}

/// `Assist/extractEvents`. Also when the person's `assist.refineEvents` is off: that setting is about
/// asking on its own when a mail is opened, not about asking when the person clicks.
pub async fn extract_events(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let args = events_args(ctx, args)?;
    let email_id = answered_email(args.email_id, !args.foreign_mails.is_empty());
    let result = assist.extract_events(&ctx.account, args).await.map_err(method_error)?;
    let mut out = Map::new();
    out.insert("accountId".into(), json!(ctx.account_id()));
    out.insert("emailId".into(), email_id);
    out.insert("events".into(), json!(result.events));
    Ok(with_source(out, &result.effective, &result.usage))
}

/// `Assist/estimate`: what one of the other calls would take, without making it.
pub async fn estimate(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let currency = currency(args)?;
    let method = arg_str(args, "method")?.ok_or_else(|| MethodError::invalid_arguments("method is required"))?;
    let arguments = match args.get("arguments") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(Value::Object(object)) => {
            // The arguments as the call would get them; an accountId in them is this one or wrong.
            if object.get("accountId").is_some_and(|id| id.as_str() != Some(ctx.account_id().as_str())) {
                return Err(MethodError::invalid_arguments("arguments/accountId is not this account"));
            }
            Value::Object(object.clone())
        }
        Some(_) => return Err(MethodError::invalid_arguments("arguments is an object")),
    };
    let call = match method {
        "Assist/compose" => EstimateArgs::Compose(compose_args(ctx, &arguments)?),
        "Assist/summarize" => EstimateArgs::Summarize(summarize_args(ctx, &arguments)?),
        "Assist/spamCheck" => EstimateArgs::SpamCheck(spam_args(ctx, &arguments)?),
        "Assist/extractEvents" => EstimateArgs::ExtractEvents(events_args(ctx, &arguments)?),
        "AssistLabel/suggest" => EstimateArgs::Suggest(suggest_args(ctx, &arguments)?),
        other => {
            return Err(MethodError::invalid_arguments(format!(
                "{other} can't be estimated; method is Assist/compose, Assist/summarize, Assist/spamCheck, \
Assist/extractEvents or AssistLabel/suggest"
            )));
        }
    };
    let estimate = assist.estimate(&ctx.account, call).await.map_err(method_error)?;
    Ok(json!({
        "accountId": ctx.account_id(),
        "method": method,
        "inputTokens": estimate.input_tokens,
        "outputTokens": estimate.output_tokens,
        "reasoningTokens": estimate.reasoning_tokens,
        "totalTokens": estimate.total_tokens(),
        "imageCount": estimate.image_count,
        "imageTokens": estimate.image_tokens,
        "calls": estimate.calls,
        "calibrated": estimate.calibrated,
        "providerId": provider_id(estimate.effective.provider_id),
        "providerName": estimate.effective.provider_name,
        "model": estimate.effective.model,
        "tokensLeftToday": estimate.tokens_left_today,
        "requestsLeftToday": estimate.requests_left_today,
        "cost": estimate_cost_json(&*assist.prices().await, estimate.cost.as_ref(), &currency),
    }))
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
    let currency = currency(args)?;
    let (rows, today) = assist.usage(&ctx.account, days).await.map_err(method_error)?;
    let prices = assist.prices().await;
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
                "reasoningTokens": row.reasoning_tokens,
                "cachedTokens": row.cached_tokens,
                "calls": row.calls,
                "cost": cost_json(&prices, row.cost_usd, &currency),
            })
        })
        .collect();
    let today = today_json(&today, &prices, &currency);
    Ok(json!({ "accountId": ctx.account_id(), "days": days, "today": today }))
}

async fn label_state(ctx: &Ctx<'_>) -> MethodResult<String> {
    Ok(ctx.jmap.store.assist_label_state(ctx.account.id).await?)
}

pub async fn label_get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    assist(ctx)?;
    let state = label_state(ctx).await?;
    let labels = ctx.jmap.store.assist_labels(ctx.account.id).await?;
    let counts = ctx.jmap.store.label_counts(ctx.account.id).await?;
    let all = labels
        .iter()
        .map(|label| (label_id(label.id), label_json(label, counts.get(&label.id).copied().unwrap_or_default())))
        .collect();
    get_answer(ctx, args, state, all)
}

fn boolean(value: &Value, property: &str) -> Result<bool, SetError> {
    value.as_bool().ok_or_else(|| SetError::invalid_properties(&[property], format!("{property} is true or false")))
}

/// A label as written, from `patch` on top of `before` (with its `counts`, which may be sent back
/// unchanged).
fn label_fields(patch: &Value, before: Option<(&AssistLabel, LabelCounts)>) -> Result<AssistLabelWrite, SetError> {
    let object = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "a label is an object"))?;
    let mut name = before.map(|(b, _)| b.name.clone());
    let mut write = match before {
        Some((before, _)) => AssistLabelWrite::of(before),
        None => AssistLabelWrite::simple(String::new(), String::new(), None),
    };
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
                write.description = match value {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    _ => return Err(SetError::invalid_properties(&["description"], "description is a string")),
                }
            }
            "color" => {
                write.color = match value {
                    Value::Null => None,
                    Value::String(text) => Some(text.to_ascii_lowercase()),
                    _ => return Err(SetError::invalid_properties(&["color"], "color is #rrggbb or null")),
                }
            }
            "rules" => {
                write.rules = Rules::check(value)
                    .map_err(|why| SetError::invalid_properties(&["rules"], why))?
                    .map(|rules| rules.to_json())
            }
            "detector" => {
                write.detector = match value {
                    Value::Null => None,
                    Value::String(name) if Detector::parse(name).is_some() => Some(name.clone()),
                    _ => {
                        return Err(SetError::invalid_properties(
                            &["detector"],
                            "detector is invoice, appointment, newsletter, shipping or null",
                        ));
                    }
                }
            }
            "learnSenders" => write.learn_senders = boolean(value, "learnSenders")?,
            "classifier" => write.classifier = boolean(value, "classifier")?,
            "id" if before.is_some() => {}
            "keyword" if before.is_some_and(|(b, _)| value.as_str() == Some(b.keyword.as_str())) => {}
            "totalEmails" if before.is_some_and(|(_, c)| value.as_i64() == Some(c.total)) => {}
            "unreadEmails" if before.is_some_and(|(_, c)| value.as_i64() == Some(c.unread)) => {}
            "examples" if before.is_some_and(|(_, c)| value.as_i64() == Some(c.examples)) => {}
            other => {
                return Err(SetError::invalid_properties(&[other], format!("{other} can't be set")));
            }
        }
    }
    write.name = name.ok_or_else(|| SetError::invalid_properties(&["name"], "a label needs a name"))?;
    Ok(write)
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
    let old_state = label_state(ctx).await?;
    if_in_state(args, &old_state)?;
    let store = &ctx.jmap.store;
    let account = &ctx.account;
    let mut response = SetResponse::default();
    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result = match label_fields(object, None) {
                Ok(write) => store.create_assist_label_with(account.id, write).await.map_err(label_store_error),
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
        let counts = store.label_counts(account.id).await?;
        for (id, patch) in update {
            let before = ctx.parse_id('g', id).and_then(|id| labels.iter().find(|label| label.id == id));
            let result = match before {
                None => Err(SetError::not_found()),
                Some(before) => {
                    let count = counts.get(&before.id).copied().unwrap_or_default();
                    match label_fields(patch, Some((before, count))) {
                        Ok(write) => store
                            .update_assist_label_with(account.id, before.id, write)
                            .await
                            .map_err(label_store_error),
                        Err(err) => Err(err),
                    }
                }
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
    let new_state = label_state(ctx).await?;
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
                "source": entry.source,
                "reason": entry.reason,
                "code": entry.code,
                "params": entry.params,
                "providerName": Some(&entry.provider).filter(|name| !name.is_empty()),
                "model": Some(&entry.model).filter(|model| !model.is_empty()),
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

/// `AssistLabel/suggest`: what the model says of every label for one mail; changes nothing.
pub async fn label_suggest(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let assist = assist(ctx)?;
    let args = suggest_args(ctx, args)?;
    let email_id = answered_email(args.email_id, !args.foreign_mails.is_empty());
    let result = assist.suggest_labels(&ctx.account, args).await.map_err(method_error)?;
    let verdicts: Vec<Value> = result
        .verdicts
        .iter()
        .map(|verdict| {
            json!({
                "labelId": verdict.label_id.map(label_id),
                "name": verdict.name,
                "reason": verdict.reason,
                "fits": verdict.fits,
                "isSet": verdict.is_set,
            })
        })
        .collect();
    let mut out = Map::new();
    out.insert("accountId".into(), json!(ctx.account_id()));
    out.insert("emailId".into(), email_id);
    out.insert("verdicts".into(), json!(verdicts));
    out.insert("newLabels".into(), json!(result.new_labels));
    Ok(with_source(out, &result.effective, &result.usage))
}
