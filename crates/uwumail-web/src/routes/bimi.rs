//! BIMI per domain (docs/bimi.md): the logo as SVG Tiny PS and an optional mark certificate,
//! served at a fixed HTTPS address on the server, the `default._bimi` record that points there,
//! and whether the domain's DMARC policy lets receivers show it.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::bimi::{self, SvgOptions};
use uwumail_smtp::dnscheck::CheckStatus;
use uwumail_store::{BimiUpdate, Domain, DomainBimi};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::session::Admin;

/// Where the logo of a domain is served, for everyone.
pub(crate) fn logo_url(web: &Web, domain: &str) -> String {
    format!("https://{}/bimi/{domain}.svg", web.settings().hostname)
}

fn certificate_url(web: &Web, domain: &str) -> String {
    format!("https://{}/bimi/{domain}.pem", web.settings().hostname)
}

/// The record the domain should publish while BIMI is on for it.
fn expected_record(web: &Web, domain: &str, bimi: &DomainBimi) -> String {
    let certificate = bimi.certificate.as_ref().map(|_| certificate_url(web, domain));
    bimi::record(&logo_url(web, domain), certificate.as_deref())
}

/// For the DNS check: the record to look for, only while BIMI is on.
pub(crate) async fn wanted_record(web: &Web, domain: &Domain) -> ApiResult<Option<String>> {
    let bimi = web.store().domain_bimi(domain.id).await?;
    Ok((bimi.enabled && bimi.svg.is_some()).then(|| expected_record(web, &domain.name, &bimi)))
}

async fn view(web: &Web, domain: &Domain) -> ApiResult<Value> {
    let bimi = web.store().domain_bimi(domain.id).await?;
    let record = expected_record(web, &domain.name, &bimi);
    let report = web.report(&domain.name);
    // What the last DNS check saw of the record, as long as it looked for the one wanted now.
    let published = report.as_ref().and_then(|report| {
        let check = report.records.iter().find(|record| record.kind == "bimi")?;
        (bimi.enabled && check.expected == record).then(|| {
            json!({
                "status": check.status,
                "found": check.found,
                "note": check.note,
                "checkedAt": report.checked_at,
            })
        })
    });
    let dmarc = match &report {
        None => json!({ "status": "unknown", "policy": null, "pct": null, "subdomainPolicy": null, "record": null }),
        Some(report) => {
            let found = report
                .records
                .iter()
                .find(|record| record.kind == "dmarc")
                .filter(|record| record.status != CheckStatus::Error)
                .map(|record| record.found.first().cloned());
            match found {
                // The lookup failed: nothing is known.
                None => {
                    json!({ "status": "unknown", "policy": null, "pct": null, "subdomainPolicy": null, "record": null })
                }
                Some(found) => json!(bimi::dmarc_fit(found.as_deref())),
            }
        }
    };
    let certificate = match &bimi.certificate {
        Some(pem) => match bimi::certificate(pem) {
            Ok((_, summary)) => {
                let now = unix_now();
                json!({
                    "kind": summary.kind,
                    "subject": summary.subject,
                    "issuer": summary.issuer,
                    "notBefore": summary.not_before,
                    "notAfter": summary.not_after,
                    "expired": summary.not_after < now,
                    "names": summary.names,
                    "coversDomain": summary.covers(&domain.name),
                    "hasLogotype": summary.has_logotype,
                })
            }
            Err(_) => Value::Null,
        },
        None => Value::Null,
    };
    Ok(json!({
        "enabled": bimi.enabled,
        "hasSvg": bimi.svg.is_some(),
        "title": bimi.title,
        "svgUpdatedAt": bimi.svg_updated_at,
        "svgBytes": bimi.svg.as_ref().map(String::len),
        "logoUrl": logo_url(web, &domain.name),
        "certificateUrl": bimi.certificate.as_ref().map(|_| certificate_url(web, &domain.name)),
        "certificate": certificate,
        "record": { "name": bimi::record_name(&domain.name), "value": record },
        "published": published,
        "dmarc": dmarc,
        "domainLogo": web.store().domain_logo(domain.id).await?.is_some(),
    }))
}

pub async fn show(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    Ok(Json(view(&web, &domain).await?))
}

fn svg_response(svg: String, cache: &'static str) -> Response {
    let mut response = Response::new(axum::body::Body::from(svg));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/svg+xml"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // Nothing in it runs, and nothing it could name is loaded.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    response
}

/// The stored logo for the portal's preview, also while BIMI is off.
pub async fn preview(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Response> {
    let domain = super::domains::load(&web, &name).await?;
    let bimi = web.store().domain_bimi(domain.id).await?;
    match bimi.svg {
        Some(svg) => Ok(svg_response(svg, "private, no-cache")),
        None => Err(ApiError::NotFound("BIMI logo".into())),
    }
}

/// `/bimi/<domain>.svg` and `/bimi/<domain>.pem`, for every receiver, while BIMI is on.
pub async fn public_file(State(web): State<Web>, Path(file): Path<String>) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "no BIMI file here\n").into_response();
    let (name, svg) = match (file.strip_suffix(".svg"), file.strip_suffix(".pem")) {
        (Some(name), _) => (name, true),
        (_, Some(name)) => (name, false),
        _ => return not_found(),
    };
    let domain = match web.store().domain(name).await {
        Ok(Some(domain)) => domain,
        Ok(None) | Err(uwumail_store::StoreError::Invalid(_)) => return not_found(),
        Err(err) => {
            tracing::error!(%err, "loading a domain for BIMI failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "try again later\n").into_response();
        }
    };
    let bimi = match web.store().domain_bimi(domain.id).await {
        Ok(bimi) if bimi.enabled => bimi,
        Ok(_) => return not_found(),
        Err(err) => {
            tracing::error!(%err, "loading BIMI failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "try again later\n").into_response();
        }
    };
    match (svg, bimi.svg, bimi.certificate) {
        (true, Some(svg), _) => svg_response(svg, "public, max-age=3600"),
        (false, _, Some(pem)) => (
            [
                (header::CONTENT_TYPE, "application/pem-certificate-chain"),
                (header::CACHE_CONTROL, "public, max-age=3600"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            pem,
        )
            .into_response(),
        _ => not_found(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    enabled: Option<bool>,
    title: Option<String>,
}

/// Switches BIMI on or off, or gives the logo another title.
pub async fn update(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<Settings>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let current = web.store().domain_bimi(domain.id).await?;
    if body.enabled == Some(true) && current.svg.is_none() {
        return Err(ApiError::Rule("bimiNoSvg", "BIMI needs a logo first".into()));
    }
    let mut update = BimiUpdate { enabled: body.enabled, ..Default::default() };
    if let Some(title) = body.title.as_deref().map(str::trim) {
        // The title lives inside the SVG, so the logo is written again with it.
        if let Some(svg) = &current.svg {
            let cleaned = bimi::tiny_ps(svg, &SvgOptions { title, background: None }).map_err(svg_error)?;
            update.svg = Some(Some(cleaned));
        } else if title.is_empty() || title.chars().count() > 100 {
            return Err(svg_error(bimi::SvgError::Title));
        }
        update.title = Some(title.to_owned());
    }
    web.store().update_domain_bimi(domain.id, update, unix_now()).await?;
    audit(&web, &session, "domain.bimi", &domain.name, json!({ "enabled": body.enabled, "title": body.title })).await;
    Ok(Json(view(&web, &domain).await?))
}

fn svg_error(error: bimi::SvgError) -> ApiError {
    let (code, detail) = error.code();
    ApiError::Rule(code, detail)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Upload {
    svg: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    background: Option<String>,
}

/// Takes an SVG, cleaned into SVG Tiny PS on the way in.
pub async fn upload(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<Upload>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let current = web.store().domain_bimi(domain.id).await?;
    // Without a title of its own or one given, the domain's name will do.
    let given = body.title.as_deref().map(str::trim).filter(|title| !title.is_empty());
    let fallback = if current.title.is_empty() { domain.name.clone() } else { current.title.clone() };
    let first =
        bimi::tiny_ps(&body.svg, &SvgOptions { title: given.unwrap_or(""), background: body.background.as_deref() });
    let cleaned = match (first, given) {
        (Err(bimi::SvgError::Title), None) => {
            bimi::tiny_ps(&body.svg, &SvgOptions { title: &fallback, background: body.background.as_deref() })
        }
        (result, _) => result,
    }
    .map_err(svg_error)?;
    let title = svg_title(&cleaned).unwrap_or(fallback);
    let bytes = cleaned.len();
    let update = BimiUpdate { title: Some(title), svg: Some(Some(cleaned)), ..Default::default() };
    web.store().update_domain_bimi(domain.id, update, unix_now()).await?;
    audit(&web, &session, "domain.bimiLogo", &domain.name, json!({ "bytes": bytes })).await;
    Ok(Json(view(&web, &domain).await?))
}

/// The title of a cleaned logo, where [`bimi::tiny_ps`] put it.
fn svg_title(svg: &str) -> Option<String> {
    let start = svg.find("<title>")? + "<title>".len();
    let end = svg[start..].find("</title>")? + start;
    Some(svg[start..end].replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
}

pub async fn remove_svg(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let update = BimiUpdate { svg: Some(None), enabled: Some(false), ..Default::default() };
    web.store().update_domain_bimi(domain.id, update, unix_now()).await?;
    audit(&web, &session, "domain.bimiLogoRemoved", &domain.name, json!({})).await;
    Ok(Json(view(&web, &domain).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateUpload {
    pem: String,
}

pub async fn upload_certificate(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<CertificateUpload>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let (pem, summary) = bimi::certificate(&body.pem).map_err(|error| {
        let (code, detail) = error.code();
        ApiError::Rule(code, detail)
    })?;
    let update = BimiUpdate { certificate: Some(Some(pem)), ..Default::default() };
    web.store().update_domain_bimi(domain.id, update, unix_now()).await?;
    audit(&web, &session, "domain.bimiCertificate", &domain.name, json!({ "kind": summary.kind })).await;
    Ok(Json(view(&web, &domain).await?))
}

pub async fn remove_certificate(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let update = BimiUpdate { certificate: Some(None), ..Default::default() };
    web.store().update_domain_bimi(domain.id, update, unix_now()).await?;
    audit(&web, &session, "domain.bimiCertificateRemoved", &domain.name, json!({})).await;
    Ok(Json(view(&web, &domain).await?))
}

/// Checks the domain's DNS now, BIMI record included.
pub async fn check(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    super::domains::run_check(&web, &domain.name).await?;
    Ok(Json(view(&web, &domain).await?))
}
