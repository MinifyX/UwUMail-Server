//! Domains: add and remove, catch-all, forwarding addresses, DNS check and DKIM key rotation.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::dnscheck::DomainSetup;
use uwumail_smtp::mta_sts::Policy;
use uwumail_store::{DkimKeyState, Domain};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

pub(crate) async fn load(web: &Web, name: &str) -> ApiResult<Domain> {
    web.store().domain(name).await?.ok_or_else(|| ApiError::NotFound(format!("domain {name}")))
}

fn report_summary(web: &Web, name: &str) -> Value {
    match web.report(name) {
        Some(report) => json!({ "status": report.status, "checkedAt": report.checked_at }),
        None => Value::Null,
    }
}

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let domains = web.store().domains().await?;
    let counts = web.store().domain_address_counts().await?;
    Ok(Json(Value::Array(
        domains
            .iter()
            .map(|domain| {
                let (people, aliases) = counts.get(&domain.name).copied().unwrap_or_default();
                json!({
                    "name": domain.name,
                    "catchAll": domain.catch_all,
                    "createdAt": domain.created_at,
                    "people": people,
                    "aliases": aliases,
                    "dns": report_summary(&web, &domain.name),
                })
            })
            .collect(),
    )))
}

pub(crate) async fn detail_json(web: &Web, name: &str) -> ApiResult<Value> {
    let domain = load(web, name).await?;
    let keys = web.store().dkim_keys(&domain.name).await?;
    let (people, aliases) = web.store().domain_address_counts().await?.get(&domain.name).copied().unwrap_or_default();
    Ok(json!({
        "name": domain.name,
        "catchAll": domain.catch_all,
        "createdAt": domain.created_at,
        "people": people,
        "aliases": aliases,
        "forwards": web.store().forward_addresses(Some(domain.name.clone())).await?,
        "keys": keys.iter().map(|key| {
            let (dns_name, dns_value) = key.dns_record();
            json!({
                "selector": key.selector,
                "algorithm": key.algorithm,
                "state": key.state(),
                "createdAt": key.created_at,
                "retiredAt": key.retired_at,
                "dnsName": dns_name,
                "dnsValue": dns_value,
            })
        }).collect::<Vec<_>>(),
        "report": web.report(&domain.name),
        "selfServiceAliases": web.store().domain_self_service(&domain.name).await?,
        "mtaSts": super::reports::mta_sts_json(web.store().mta_sts(&domain.name).await?),
        "setup": {
            "hostname": web.settings().hostname,
            "relayHost": web.smtp().relay_host(),
            "upstreamMx": web.smtp().behind_upstream_server(),
        },
    }))
}

pub async fn detail(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(detail_json(&web, &name).await?))
}

#[derive(Deserialize)]
pub struct NewDomain {
    name: String,
}

pub async fn create(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewDomain>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let domain = web.store().create_domain(new.name.trim()).await?;
    if let Err(err) = uwumail_smtp::dkim::ensure_domain_keys(web.store(), &domain.name).await {
        tracing::error!(%err, domain = %domain.name, "creating DKIM keys failed");
        return Err(ApiError::Internal);
    }
    audit(&web, &session, "domain.create", &domain.name, json!({})).await;
    Ok((StatusCode::CREATED, Json(detail_json(&web, &domain.name).await?)))
}

pub async fn remove(State(web): State<Web>, Admin(session): Admin, Path(name): Path<String>) -> ApiResult<StatusCode> {
    let domain = load(&web, &name).await?;
    let (people, aliases) = web.store().domain_address_counts().await?.get(&domain.name).copied().unwrap_or_default();
    let forwards = web.store().forward_addresses(Some(domain.name.clone())).await?.len() as i64;
    let in_use = people + aliases + forwards;
    if in_use > 0 {
        return Err(ApiError::Rule("domainInUse", format!("{in_use} addresses still use {}", domain.name)));
    }
    web.store().delete_domain(&domain.name).await?;
    web.forget_report(&domain.name);
    audit(&web, &session, "domain.remove", &domain.name, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct CatchAll {
    login: Option<String>,
}

pub async fn set_catch_all(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(catch_all): Json<CatchAll>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let login = catch_all.login.filter(|login| !login.trim().is_empty());
    if let Some(login) = &login {
        let account = web.store().account(login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
        if account.deleted_at.is_some() {
            return Err(ApiError::Invalid(format!("{login} is in the trash")));
        }
    }
    web.store().set_catch_all(&domain.name, login.as_deref()).await?;
    audit(&web, &session, "domain.catchAll", &domain.name, json!({ "account": login })).await;
    Ok(Json(detail_json(&web, &domain.name).await?))
}

/// Checks the domain's DNS records now and keeps the result for the overview.
#[derive(Deserialize)]
pub struct ForwardAddressBody {
    /// The part before the @; the domain is the one in the path.
    local: String,
    targets: Vec<String>,
    #[serde(default)]
    note: String,
}

/// Creates a forwarding address of this domain or replaces its targets.
pub async fn set_forward_address(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<ForwardAddressBody>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let local = body.local.trim();
    if local.is_empty() || local.contains('@') {
        return Err(ApiError::Invalid("give the part before the @".into()));
    }
    let address = format!("{local}@{}", domain.name);
    let saved = web.store().set_forward_address(&address, body.targets, &body.note).await?;
    audit(&web, &session, "domain.forwardAddress", &saved.address, json!({ "targets": saved.targets })).await;
    Ok(Json(detail_json(&web, &domain.name).await?))
}

pub async fn remove_forward_address(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let address = format!("{local}@{}", domain.name);
    web.store().remove_forward_address(&address).await?;
    audit(&web, &session, "domain.forwardAddressRemove", &address, json!({})).await;
    Ok(Json(detail_json(&web, &domain.name).await?))
}

pub async fn check(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let report = run_check(&web, &domain.name).await?;
    Ok(Json(json!(report)))
}

pub async fn run_check(web: &Web, domain: &str) -> ApiResult<uwumail_smtp::dnscheck::DomainReport> {
    let Some(checker) = web.dns() else {
        return Err(ApiError::Rule("dnsUnavailable", "the server has no working DNS resolver".into()));
    };
    let keys = web.store().dkim_keys(domain).await?;
    let relay_host = web.smtp().relay_host();
    let policy = web.store().mta_sts(domain).await?.map(|settings| Policy::ours(settings.mode, &settings.mx));
    let report = checker
        .check(DomainSetup {
            domain,
            hostname: &web.settings().hostname,
            relay_host: relay_host.as_deref(),
            upstream_mx: web.smtp().behind_upstream_server(),
            dkim_keys: &keys,
            mta_sts: policy.as_ref(),
        })
        .await;
    web.keep_report(report.clone());
    Ok(report)
}

pub async fn rotate_keys(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let keys = uwumail_smtp::dkim::prepare_rotation(web.store(), &domain.name).await.map_err(|err| {
        tracing::error!(%err, domain = %domain.name, "preparing new DKIM keys failed");
        ApiError::Internal
    })?;
    let selectors: Vec<_> =
        keys.iter().filter(|key| key.state() == DkimKeyState::Pending).map(|key| key.selector.clone()).collect();
    audit(&web, &session, "domain.dkimPrepare", &domain.name, json!({ "selectors": selectors })).await;
    web.forget_report(&domain.name);
    Ok(Json(detail_json(&web, &domain.name).await?))
}

#[derive(Deserialize, Default)]
pub struct Activation {
    /// Switch even if the DNS check does not see the new keys yet.
    #[serde(default)]
    force: bool,
}

pub async fn activate_keys(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    body: Option<Json<Activation>>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let force = body.map(|Json(activation)| activation.force).unwrap_or_default();
    if !force {
        // Signing with a key nobody can look up would break DKIM for every mail.
        let report = run_check(&web, &domain.name).await?;
        let unpublished = report.records.iter().any(|record| {
            record.key_state == Some(DkimKeyState::Pending) && record.status != uwumail_smtp::dnscheck::CheckStatus::Ok
        });
        if unpublished {
            return Err(ApiError::Rule(
                "keysNotPublished",
                "the DNS records of the new keys are not visible yet".into(),
            ));
        }
    }
    web.store().activate_dkim_keys(&domain.name).await?;
    audit(&web, &session, "domain.dkimActivate", &domain.name, json!({ "forced": force })).await;
    web.forget_report(&domain.name);
    Ok(Json(detail_json(&web, &domain.name).await?))
}

pub async fn remove_key(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, selector)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    web.store().remove_dkim_key(&domain.name, &selector).await?;
    audit(&web, &session, "domain.dkimRemove", &domain.name, json!({ "selector": selector })).await;
    web.forget_report(&domain.name);
    Ok(Json(detail_json(&web, &domain.name).await?))
}

#[derive(Deserialize)]
pub struct CloudflareRequest {
    token: String,
    /// Kinds of wrong records to overwrite: mx, spf, dmarc, dkim.
    #[serde(default)]
    replace: Vec<String>,
}

/// Puts the missing records into Cloudflare with a token that is used once and forgotten.
pub async fn cloudflare(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(request): Json<CloudflareRequest>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    if request.token.trim().is_empty() {
        return Err(ApiError::Invalid("a Cloudflare API token is needed".into()));
    }
    let report = run_check(&web, &domain.name).await?;
    let wanted = crate::cloudflare::wanted_records(&report);
    let results = crate::cloudflare::Cloudflare::new(&request.token)
        .apply(&domain.name, &wanted, &request.replace)
        .await
        .map_err(|message| ApiError::Rule("cloudflareFailed", message))?;
    let changed: Vec<String> = results
        .iter()
        .filter(|result| matches!(result.outcome, "created" | "updated"))
        .map(|result| format!("{} {}", result.record_type, result.name))
        .collect();
    audit(&web, &session, "domain.cloudflare", &domain.name, json!({ "changed": changed })).await;
    web.forget_report(&domain.name);
    Ok(Json(json!({ "results": results })))
}
