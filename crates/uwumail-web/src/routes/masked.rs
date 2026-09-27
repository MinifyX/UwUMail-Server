//! My account → Masked addresses: random addresses one makes per website (docs/jmap-masked-email.md).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{MaskedState, MaskedUpdate, NewMaskedAddress};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

async fn overview(web: &Web, account_id: i64) -> ApiResult<Value> {
    Ok(json!({
        "addresses": web.store().masked_addresses(account_id, None).await?,
        "domains": web.store().masked_domains().await?,
    }))
}

pub async fn list(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(overview(&web, session.account.id).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMasked {
    domain: Option<String>,
    #[serde(default)]
    for_domain: String,
    #[serde(default)]
    description: String,
    url: Option<String>,
    email_prefix: Option<String>,
}

pub async fn create(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewMasked>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // Made by hand, it is meant to be used: it does not wait a day for mail like one an app makes.
    let created = web
        .store()
        .create_masked_address(
            session.account.id,
            NewMaskedAddress {
                domain: new.domain.filter(|domain| !domain.trim().is_empty()),
                state: Some(MaskedState::Enabled),
                for_domain: new.for_domain,
                description: new.description,
                url: new.url,
                email_prefix: new.email_prefix,
                created_by: "Portal".into(),
            },
        )
        .await?;
    tracing::info!(login = %session.account.login, masked = %created.email, "created a masked address");
    Ok((StatusCode::CREATED, Json(json!(created))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedChanges {
    state: Option<String>,
    for_domain: Option<String>,
    description: Option<String>,
    url: Option<String>,
}

fn state(value: &str) -> ApiResult<MaskedState> {
    match MaskedState::parse(value) {
        Some(state) if state != MaskedState::Pending => Ok(state),
        _ => Err(ApiError::Invalid("the state is enabled, disabled or deleted".into())),
    }
}

pub async fn update(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(changes): Json<MaskedChanges>,
) -> ApiResult<Json<Value>> {
    let update = MaskedUpdate {
        state: changes.state.as_deref().map(state).transpose()?,
        for_domain: changes.for_domain,
        description: changes.description,
        url: changes.url.map(|url| Some(url).filter(|url| !url.trim().is_empty())),
    };
    let updated = web.store().update_masked_address(session.account.id, id, update).await?;
    Ok(Json(json!(updated)))
}

/// Deletes a masked address: it refuses mail from now on, and nobody gets it again.
pub async fn delete(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let update = MaskedUpdate { state: Some(MaskedState::Deleted), ..Default::default() };
    web.store().update_masked_address(session.account.id, id, update).await?;
    tracing::info!(login = %session.account.login, id, "deleted a masked address");
    Ok(Json(overview(&web, session.account.id).await?))
}
