//! "Addresses and storage" in My account, and the admin switches that open it up.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::MailboxRole;

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{Admin, Session};

pub async fn addresses(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(json!(web.store().own_addresses(session.account.id).await?)))
}

#[derive(Deserialize)]
pub struct NewAlias {
    address: String,
}

pub async fn create_alias(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewAlias>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let created = web.store().create_own_alias(session.account.id, new.address.trim()).await?;
    tracing::info!(login = %session.account.login, alias = %created.address, "created an own alias");
    Ok((StatusCode::CREATED, Json(json!(web.store().own_addresses(session.account.id).await?))))
}

pub async fn delete_alias(
    State(web): State<Web>,
    session: Session,
    Path(address): Path<String>,
) -> ApiResult<Json<Value>> {
    web.store().delete_own_alias(session.account.id, &address).await?;
    tracing::info!(login = %session.account.login, alias = %address, "deleted an own alias");
    Ok(Json(json!(web.store().own_addresses(session.account.id).await?)))
}

pub async fn storage(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let account = web.store().account_by_id(session.account.id).await?.unwrap_or(session.account);
    Ok(Json(json!({
        "usedBytes": account.used_bytes,
        "quotaBytes": account.quota_bytes,
        "mailboxes": web.store().mailbox_usage(account.id).await?,
    })))
}

pub async fn empty_mailbox(
    State(web): State<Web>,
    session: Session,
    Path(role): Path<String>,
) -> ApiResult<Json<Value>> {
    let role = match role.as_str() {
        "trash" => MailboxRole::Trash,
        "junk" => MailboxRole::Junk,
        _ => return Err(ApiError::Rule("notEmptiable", "only Trash and Junk can be emptied".into())),
    };
    let removed = web.store().empty_mailbox(session.account.id, role).await?;
    tracing::info!(login = %session.account.login, role = role.as_str(), removed, "emptied a folder");
    Ok(Json(json!({ "removed": removed })))
}

#[derive(Deserialize)]
pub struct SelfService {
    on: bool,
}

pub async fn set_domain_self_service(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<SelfService>,
) -> ApiResult<StatusCode> {
    web.store().set_domain_self_service(&name, body.on).await?;
    audit(&web, &session, "domain.selfServiceAliases", &name, json!({ "on": body.on })).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AliasLimit {
    limit: i64,
}

pub async fn set_alias_limit(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(body): Json<AliasLimit>,
) -> ApiResult<StatusCode> {
    let account = web.store().account(&login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
    web.store().set_alias_limit(account.id, body.limit).await?;
    audit(&web, &session, "account.aliasLimit", &account.login, json!({ "limit": body.limit })).await;
    Ok(StatusCode::NO_CONTENT)
}
