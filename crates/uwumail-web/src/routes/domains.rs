//! Domains: add and remove, their kind and masked address policy, catch-all, forwarding addresses,
//! DNS check and DKIM key rotation.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::dnscheck::DomainSetup;
use uwumail_smtp::mta_sts::Policy;
use uwumail_store::{DkimKeyState, Domain, DomainKind, DomainMaskedPolicy, MaskedMode};

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
                    "kind": domain.kind,
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
    // A mail domain has a policy for its users and may become masked-only once nothing is in the
    // way; a masked-only domain says which policies name it, as turning it back takes it out of them.
    let (policy, blockers, used_by) = match domain.kind {
        DomainKind::Mail => (
            json!(web.store().domain_masked_policy(&domain.name).await?),
            json!(web.store().domain_kind_blockers(&domain.name).await?),
            Value::Null,
        ),
        DomainKind::Masked => {
            let (domains, accounts) = web.store().masked_domain_users(&domain.name).await?;
            (Value::Null, Value::Null, json!({ "domains": domains, "accounts": accounts }))
        }
    };
    Ok(json!({
        "name": domain.name,
        "kind": domain.kind,
        "catchAll": domain.catch_all,
        "createdAt": domain.created_at,
        "people": people,
        "aliases": aliases,
        "forwards": web.store().forward_addresses(Some(domain.name.clone())).await?,
        "groups": web.store().groups(Some(domain.name.clone())).await?,
        "maskedPolicy": policy,
        // The masked-only domains a policy can name.
        "maskedDomainChoices": masked_domain_names(web).await?,
        "kindBlockers": blockers,
        "maskedUsedBy": used_by,
        // Masked addresses people made on the domain that still take mail; they keep it in use.
        "maskedInUse": web.store().domain_masked_address_count(&domain.name).await?,
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

/// The names of the masked-only domains.
pub(crate) async fn masked_domain_names(web: &Web) -> ApiResult<Vec<String>> {
    Ok(web
        .store()
        .domains()
        .await?
        .into_iter()
        .filter(|domain| domain.kind == DomainKind::Masked)
        .map(|domain| domain.name)
        .collect())
}

#[derive(Deserialize)]
pub struct NewDomain {
    name: String,
    /// A mail domain unless it says otherwise.
    #[serde(default)]
    kind: DomainKind,
}

pub async fn create(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewDomain>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let domain = web.store().create_domain_with_kind(new.name.trim(), new.kind).await?;
    if let Err(err) = uwumail_smtp::dkim::ensure_domain_keys(web.store(), &domain.name).await {
        tracing::error!(%err, domain = %domain.name, "creating DKIM keys failed");
        return Err(ApiError::Internal);
    }
    audit(&web, &session, "domain.create", &domain.name, json!({ "kind": domain.kind })).await;
    Ok((StatusCode::CREATED, Json(detail_json(&web, &domain.name).await?)))
}

pub async fn remove(State(web): State<Web>, Admin(session): Admin, Path(name): Path<String>) -> ApiResult<StatusCode> {
    let domain = load(&web, &name).await?;
    let (people, aliases) = web.store().domain_address_counts().await?.get(&domain.name).copied().unwrap_or_default();
    let forwards = web.store().forward_addresses(Some(domain.name.clone())).await?.len() as i64;
    let groups = web.store().groups(Some(domain.name.clone())).await?.len() as i64;
    let masked = web.store().domain_masked_address_count(&domain.name).await?;
    let in_use = people + aliases + forwards + groups + masked;
    if in_use > 0 {
        return Err(ApiError::Rule("domainInUse", format!("{in_use} addresses still use {}", domain.name)));
    }
    web.store().delete_domain(&domain.name).await?;
    web.forget_report(&domain.name);
    audit(&web, &session, "domain.remove", &domain.name, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct KindBody {
    kind: DomainKind,
}

/// Makes a mail domain masked-only (only while nothing but masked addresses is on it), or a
/// masked-only domain a mail domain again, which takes it out of every policy that named it.
pub async fn set_kind(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<KindBody>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    if body.kind == DomainKind::Masked {
        let blockers = web.store().domain_kind_blockers(&domain.name).await?;
        if !blockers.is_empty() {
            let detail = format!("{} still has addresses or settings that are not masked addresses", domain.name);
            return Err(ApiError::Blocked("kindChangeBlocked", detail, json!(blockers)));
        }
    }
    let change = web.store().set_domain_kind(&domain.name, body.kind).await?;
    if change.kind != domain.kind {
        let details = json!({
            "kind": change.kind,
            "removedFromDomains": change.removed_from_domains,
            "removedFromAccounts": change.removed_from_accounts,
        });
        audit(&web, &session, "domain.kind", &domain.name, details).await;
    }
    Ok(Json(detail_json(&web, &domain.name).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskedPolicyBody {
    mode: MaskedMode,
    #[serde(default)]
    masked_domains: Vec<String>,
    default_domain: Option<String>,
}

/// Where the users of a mail domain may make masked addresses.
pub async fn set_masked_policy(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<MaskedPolicyBody>,
) -> ApiResult<Json<Value>> {
    let domain = load(&web, &name).await?;
    let policy = DomainMaskedPolicy {
        mode: body.mode,
        masked_domains: body.masked_domains,
        default_domain: body.default_domain.filter(|name| !name.trim().is_empty()),
    };
    let saved = web.store().set_domain_masked_policy(&domain.name, policy).await?;
    audit(&web, &session, "domain.maskedPolicy", &domain.name, json!(saved)).await;
    Ok(Json(detail_json(&web, &domain.name).await?))
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
    let certificate = web.settings().certificate.as_ref().and_then(|source| source());
    // A self-signed stand-in says nothing about the account the real one comes from, nor about
    // the key it will have.
    let lasting = certificate.filter(|status| !(status.automatic && status.self_signed));
    let account = lasting.as_ref().and_then(|status| status.lets_encrypt_account.clone().filter(|_| status.automatic));
    let chain = lasting.map(|status| status.chain).filter(|chain| !chain.is_empty());
    let report = checker
        .check(DomainSetup {
            domain,
            hostname: &web.settings().hostname,
            relay_host: relay_host.as_deref(),
            upstream_mx: web.smtp().behind_upstream_server(),
            dkim_keys: &keys,
            mta_sts: policy.as_ref(),
            lets_encrypt_account: account.as_deref(),
            certificate: chain.as_deref(),
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
    /// Kinds of wrong records to overwrite: mx, spf, dmarc, dkim. `caa` is also the only way a
    /// missing CAA record is created: it decides who may issue certificates for the host name.
    #[serde(default)]
    replace: Vec<String>,
    /// Kinds of working records to rewrite the way UwUMail would publish them.
    #[serde(default)]
    tidy: Vec<String>,
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
        .apply(&domain.name, &wanted, &request.replace, &request.tidy)
        .await
        .map_err(|message| ApiError::Rule("cloudflareFailed", message))?;
    let changed: Vec<String> = results
        .iter()
        .filter(|result| matches!(result.outcome, "created" | "updated" | "requoted"))
        .map(|result| format!("{} {}", result.record_type, result.name))
        .collect();
    audit(&web, &session, "domain.cloudflare", &domain.name, json!({ "changed": changed })).await;
    web.forget_report(&domain.name);
    Ok(Json(json!({ "results": results })))
}
