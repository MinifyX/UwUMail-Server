//! The AI assistant in the portal (docs/llm.md): the admin's providers, policy and usage under
//! Server → Settings → AI assistant, and a person's own providers, choices and usage under My account.
//!
//! Keys go in and never come out: no answer here carries one, only its last four characters.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_assist::{Assist, AssistError, KINDS, ProviderInput, SettingsPatch};
use uwumail_store::{AssistPolicy, UsageRow};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{Admin, Session};

fn assist(web: &Web) -> ApiResult<&Assist> {
    web.assist().ok_or_else(|| ApiError::NotFound("the AI assistant is not set up here".into()))
}

fn api_error(err: AssistError) -> ApiError {
    match err {
        AssistError::Invalid { code, description, .. } => ApiError::Rule(code, description),
        AssistError::Forbidden(description) => ApiError::Rule("assistNotAllowed", description),
        AssistError::NotFound(what) => ApiError::NotFound(what),
        AssistError::Unavailable(description) => ApiError::Rule("assistUnavailable", description),
        AssistError::OverQuota(description) => ApiError::Rule("overQuota", description),
        AssistError::ProviderFailed { description, .. } => ApiError::Rule("providerFailed", description),
        AssistError::Busy => ApiError::Busy,
        AssistError::Store(err) => err.into(),
    }
}

#[derive(Deserialize)]
pub struct Days {
    days: Option<u32>,
}

fn since(days: Option<u32>) -> String {
    let days = i64::from(days.unwrap_or(30).clamp(1, 400));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    uwumail_store::utc_day(now - (days - 1) * 86_400)
}

fn usage_rows(rows: &[UsageRow], with_login: bool) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let mut value = json!({
                "day": row.day,
                "providerId": row.provider_id,
                "providerName": row.provider_name.clone().unwrap_or_else(|| format!("#{}", row.provider_id)),
                "feature": row.feature,
                "requests": row.requests,
                "inputTokens": row.input_tokens,
                "outputTokens": row.output_tokens,
            });
            if with_login {
                value["login"] = json!(row.login);
            }
            value
        })
        .collect()
}

/// What an admin's change says in the change log: never the key.
fn logged(input: &ProviderInput) -> Value {
    json!({
        "name": input.name,
        "kind": input.kind,
        "baseUrl": input.base_url,
        "keyChanged": input.api_key.is_some(),
        "model": input.model,
        "fastModel": input.fast_model,
        "enabled": input.enabled,
        "access": input.access,
        "domains": input.domains,
        "people": input.people,
        "features": input.features,
        "requestsPerDay": input.requests_per_day,
        "tokensPerDay": input.tokens_per_day,
    })
}

pub async fn admin_view(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let assist = assist(&web)?;
    let policy = web.store().assist_policy().await?;
    let providers = assist.admin_providers().await.map_err(api_error)?;
    Ok(Json(json!({ "policy": policy, "providers": providers, "kinds": KINDS })))
}

pub async fn set_policy(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(mut policy): Json<AssistPolicy>,
) -> ApiResult<Json<Value>> {
    assist(&web)?;
    policy.allow_personal_private &= policy.allow_personal;
    web.store().set_assist_policy(policy.clone()).await?;
    audit(&web, &session, "assist.policy", "server", json!(policy)).await;
    Ok(Json(json!(policy)))
}

pub async fn create_provider(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(input): Json<ProviderInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let details = logged(&input);
    let view = assist(&web)?.create_server_provider(input).await.map_err(api_error)?;
    audit(&web, &session, "assist.provider.create", &view.name, details).await;
    Ok((StatusCode::CREATED, Json(json!(view))))
}

pub async fn update_provider(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
    Json(input): Json<ProviderInput>,
) -> ApiResult<Json<Value>> {
    let details = logged(&input);
    let view = assist(&web)?.update_server_provider(id, input).await.map_err(api_error)?;
    audit(&web, &session, "assist.provider.update", &view.name, details).await;
    Ok(Json(json!(view)))
}

pub async fn delete_provider(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let assist = assist(&web)?;
    let name = web.store().assist_provider(id).await?.map(|p| p.name).unwrap_or_default();
    assist.delete_server_provider(id).await.map_err(api_error)?;
    audit(&web, &session, "assist.provider.delete", &name, json!({ "id": id })).await;
    Ok(StatusCode::NO_CONTENT)
}

fn models_json(models: Vec<(String, String)>, model: Option<String>, fast: Option<String>) -> Value {
    let models: Vec<Value> = models.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect();
    json!({ "models": models, "model": model, "fastModel": fast })
}

pub async fn admin_models(State(web): State<Web>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (models, model, fast) = assist(&web)?.models(None, id).await.map_err(api_error)?;
    Ok(Json(models_json(models, model, fast)))
}

pub async fn admin_usage(State(web): State<Web>, _admin: Admin, Query(days): Query<Days>) -> ApiResult<Json<Value>> {
    assist(&web)?;
    let rows = web.store().assist_usage(None, since(days.days)).await?;
    Ok(Json(json!({ "days": usage_rows(&rows, true) })))
}

pub async fn account_view(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let assist = assist(&web)?;
    let account = &session.account;
    let capability = assist.capability(account).await.map_err(api_error)?;
    let providers = assist.providers(account).await.map_err(api_error)?;
    let settings = assist.settings(account).await.map_err(api_error)?;
    let today = assist.today(account).await.map_err(api_error)?;
    let labels = web.store().assist_labels(account.id).await?.len();
    Ok(Json(json!({
        "features": capability.features,
        "mayAddProviders": capability.may_add_providers,
        "mayUsePrivateAddresses": capability.may_use_private_addresses,
        "maxProviders": capability.max_providers,
        "providers": providers,
        "settings": settings,
        "today": today,
        "kinds": KINDS,
        "labels": labels,
    })))
}

pub async fn create_own_provider(
    State(web): State<Web>,
    session: Session,
    Json(input): Json<ProviderInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let input = ProviderInput { enabled: None, access: None, domains: None, people: None, features: None, ..input };
    let input = ProviderInput { requests_per_day: None, tokens_per_day: None, ..input };
    let view = assist(&web)?.create_personal_provider(&session.account, input).await.map_err(api_error)?;
    Ok((StatusCode::CREATED, Json(json!(view))))
}

pub async fn update_own_provider(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(input): Json<ProviderInput>,
) -> ApiResult<Json<Value>> {
    let input = ProviderInput { enabled: None, access: None, domains: None, people: None, features: None, ..input };
    let input = ProviderInput { requests_per_day: None, tokens_per_day: None, ..input };
    let view = assist(&web)?.update_personal_provider(&session.account, id, input).await.map_err(api_error)?;
    Ok(Json(json!(view)))
}

pub async fn delete_own_provider(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    assist(&web)?.delete_personal_provider(&session.account, id).await.map_err(api_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn own_models(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (models, model, fast) = assist(&web)?.models(Some(&session.account), id).await.map_err(api_error)?;
    Ok(Json(models_json(models, model, fast)))
}

pub async fn chatgpt_login(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (user_code, verification_uri, interval, expires_at) =
        assist(&web)?.chatgpt_login(&session.account, id).await.map_err(api_error)?;
    Ok(Json(json!({
        "userCode": user_code,
        "verificationUri": verification_uri,
        "interval": interval,
        "expiresAt": expires_at,
    })))
}

pub async fn chatgpt_poll(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let (status, description) = assist(&web)?.chatgpt_poll(&session.account, id).await.map_err(api_error)?;
    let mut out = json!({ "status": status });
    if let Some(description) = description {
        out["description"] = json!(description);
    }
    Ok(Json(out))
}

pub async fn set_settings(
    State(web): State<Web>,
    session: Session,
    Json(patch): Json<SettingsPatch>,
) -> ApiResult<Json<Value>> {
    let view = assist(&web)?.set_settings(&session.account, patch).await.map_err(api_error)?;
    Ok(Json(json!(view)))
}

pub async fn account_usage(
    State(web): State<Web>,
    session: Session,
    Query(days): Query<Days>,
) -> ApiResult<Json<Value>> {
    let assist = assist(&web)?;
    let rows = web.store().assist_usage(Some(session.account.id), since(days.days)).await?;
    let today = assist.today(&session.account).await.map_err(api_error)?;
    Ok(Json(json!({ "days": usage_rows(&rows, false), "today": today })))
}
