//! Signatures per domain (docs/signatures.md): a person's own in "My mailbox", the company
//! signature of a domain on its admin page.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uwumail_store::{
    CompanySignature, CompanySignatureMode, IDENTITY_SIGNATURE_MAX_BYTES, MAX_SIGNATURE_CHANGES,
    SIGNATURE_PLACEHOLDERS, SignatureChanges, SignatureText,
};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{Admin, Session};

async fn overview(web: &Web, account_id: i64) -> ApiResult<Value> {
    let overview = web.store().signature_overview(account_id).await?;
    let mut value = serde_json::to_value(&overview).map_err(|_| ApiError::Internal)?;
    value["limits"] = json!({
        "maxSize": IDENTITY_SIGNATURE_MAX_BYTES,
        "maxChanges": MAX_SIGNATURE_CHANGES,
        "placeholders": SIGNATURE_PLACEHOLDERS,
    });
    Ok(value)
}

/// The person's signatures: per domain, for every domain, per address, and what each address
/// sends with.
pub async fn show(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(overview(&web, session.account.id).await?))
}

/// One signature in a change: `null` removes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureBody {
    #[serde(default)]
    text: String,
    #[serde(default)]
    html: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignaturesChange {
    /// By domain, or `*` for every domain.
    #[serde(default)]
    domains: Map<String, Value>,
    /// By identity id.
    #[serde(default)]
    identities: Map<String, Value>,
}

fn signature(value: Value, what: &str) -> ApiResult<Option<SignatureText>> {
    if value.is_null() {
        return Ok(None);
    }
    let body: SignatureBody =
        serde_json::from_value(value).map_err(|err| ApiError::Invalid(format!("{what}: {err}")))?;
    Ok(Some(SignatureText::new(body.text, body.html)))
}

/// Sets or removes signatures, all at once or none.
pub async fn update(
    State(web): State<Web>,
    session: Session,
    Json(change): Json<SignaturesChange>,
) -> ApiResult<Json<Value>> {
    let mut changes = SignatureChanges::default();
    for (domain, value) in change.domains {
        let signature = signature(value, &domain)?;
        changes.domains.push((domain, signature));
    }
    for (id, value) in change.identities {
        let number = id.parse::<i64>().map_err(|_| ApiError::Invalid(format!("{id} is no identity")))?;
        changes.identities.push((number, signature(value, &id)?));
    }
    web.store().set_signatures(session.account.id, changes).await?;
    Ok(Json(overview(&web, session.account.id).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompanySignatureBody {
    mode: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    html: String,
}

/// The company signature of a domain.
pub async fn show_domain(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    Ok(Json(json!(web.store().domain_signature(&domain.name).await?)))
}

/// Sets the company signature of a domain: off, a template, or a footer the server appends.
pub async fn update_domain(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<CompanySignatureBody>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let mode = CompanySignatureMode::parse(&body.mode)
        .ok_or_else(|| ApiError::Invalid("mode is off, template or footer".into()))?;
    if mode == CompanySignatureMode::Footer && body.text.trim().is_empty() && body.html.trim().is_empty() {
        return Err(ApiError::Invalid("a footer needs a text".into()));
    }
    let signature = CompanySignature { mode, text: body.text, html: body.html };
    web.store().set_domain_signature(&domain.name, signature).await?;
    audit(&web, &session, "domain.signature", &domain.name, json!({ "mode": mode.as_str() })).await;
    Ok(Json(json!(web.store().domain_signature(&domain.name).await?)))
}
