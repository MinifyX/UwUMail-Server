//! "Forwarding and away" in My account, and the public page where the owner of an address
//! agrees to receive someone's forwarded mail.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::Language;
use uwumail_store::{Account, NewQueueRecipient, SecurityEvent, VacationResponse};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::notices::{Notice, Origin, notify};
use crate::session::{Admin, Session};

/// Confirmation mails go to addresses a person typed in, so they must never turn into a way to
/// flood strangers: each address gets at most one an hour, each person sends a few an hour.
const CONFIRMATIONS_PER_HOUR: i64 = 5;
const CONFIRMATIONS_PER_DAY: i64 = 20;
const CONFIRMATION_EVENT: &str = "forwardingConfirmationSent";

async fn check_confirmation_allowed(web: &Web, account_id: i64, address: &str) -> ApiResult<()> {
    let now = unix_now();
    let store = web.store();
    let throttled = || ApiError::Rule("forwardingThrottled", "too many confirmation mails, try again later".into());
    if store.count_security_events(account_id, CONFIRMATION_EVENT, now - 3600, Some(address)).await? > 0 {
        return Err(throttled());
    }
    if store.count_security_events(account_id, CONFIRMATION_EVENT, now - 3600, None).await? >= CONFIRMATIONS_PER_HOUR
        || store.count_security_events(account_id, CONFIRMATION_EVENT, now - 24 * 3600, None).await?
            >= CONFIRMATIONS_PER_DAY
    {
        return Err(throttled());
    }
    Ok(())
}

async fn forwarding_json(web: &Web, account_id: i64) -> ApiResult<Value> {
    let forwarding = web.store().forwarding(account_id).await?;
    Ok(json!({
        "keepCopy": forwarding.keep_copy,
        "externalAllowed": web.smtp().allow_external_forwarding() && !forwarding.external_blocked,
        "targets": forwarding.targets,
        "maxTargets": uwumail_store::MAX_FORWARD_TARGETS,
    }))
}

pub async fn forwarding(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(forwarding_json(&web, session.account.id).await?))
}

#[derive(Deserialize)]
pub struct NewTarget {
    address: String,
}

pub async fn add_target(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewTarget>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (local, domain) = uwumail_store::normalize_address(&new.address)?;
    let address = format!("{local}@{domain}");
    if web.store().resolve_recipient(&address).await?.is_none() {
        check_confirmation_allowed(&web, session.account.id, &address).await?;
    }
    let allowed = web.smtp().allow_external_forwarding();
    let (target, token) = web.store().add_forward_target(session.account.id, &address, allowed).await?;
    if let Some(token) = &token {
        send_confirmation(&web, &session.account, &target.address, token).await?;
        let event = SecurityEvent {
            kind: CONFIRMATION_EVENT.into(),
            actor: String::new(),
            ip: session.client.ip.to_string(),
            details: json!({ "address": target.address }),
        };
        if let Err(err) = web.store().record_security_event(session.account.id, event).await {
            tracing::error!(%err, "writing the security activity failed");
        }
    }
    let ip = session.client.ip.to_string();
    let notice = Notice::ForwardingAdded { address: target.address.clone() };
    notify(&web, &session.account, notice, Origin { actor: "", ip: &ip }).await;
    Ok((StatusCode::CREATED, Json(forwarding_json(&web, session.account.id).await?)))
}

pub async fn remove_target(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let removed = web.store().remove_forward_target(session.account.id, id).await?;
    let event = SecurityEvent {
        kind: "forwardingRemoved".into(),
        actor: String::new(),
        ip: session.client.ip.to_string(),
        details: json!({ "address": removed.address }),
    };
    if let Err(err) = web.store().record_security_event(session.account.id, event).await {
        tracing::error!(%err, "writing the security activity failed");
    }
    Ok(Json(forwarding_json(&web, session.account.id).await?))
}

#[derive(Deserialize)]
pub struct KeepCopy {
    keep: bool,
}

pub async fn set_keep_copy(
    State(web): State<Web>,
    session: Session,
    Json(body): Json<KeepCopy>,
) -> ApiResult<Json<Value>> {
    web.store().set_forward_keep_copy(session.account.id, body.keep).await?;
    Ok(Json(forwarding_json(&web, session.account.id).await?))
}

fn language(web: &Web, preferences: &serde_json::Map<String, Value>) -> Language {
    match preferences.get("language").and_then(Value::as_str) {
        Some("de") => Language::De,
        Some("en") => Language::En,
        _ => web.smtp().tone().language,
    }
}

/// Asks the owner of `target` whether they want the forwarded mail. Nothing is forwarded before.
async fn send_confirmation(web: &Web, account: &Account, target: &str, token: &str) -> ApiResult<()> {
    let preferences = web.store().preferences(account.id).await.unwrap_or_default();
    let hostname = &web.settings().hostname;
    let link = format!("https://{hostname}/forwarding/{token}");
    let login = &account.login;
    let (subject, body) = match language(web, &preferences) {
        Language::De => (
            format!("Weiterleitung bestätigen: {login}"),
            format!(
                "Hallo,\n\n{login} möchte Mails an diese Adresse ({target}) weiterleiten.\n\n\
                 Wenn das in Ordnung ist, bestätige es hier:\n{link}\n\n\
                 Wenn nicht, musst du nichts tun: Ohne Bestätigung wird nichts weitergeleitet. \
                 Der Link gilt sieben Tage.\n\nUwUMail auf {hostname}\n"
            ),
        ),
        Language::En => (
            format!("Confirm forwarding: {login}"),
            format!(
                "Hello,\n\n{login} would like to forward mail to this address ({target}).\n\n\
                 If that is fine with you, confirm it here:\n{link}\n\n\
                 If not, you don't need to do anything: nothing is forwarded without confirmation. \
                 The link works for seven days.\n\nUwUMail on {hostname}\n"
            ),
        ),
    };
    let domain = login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(hostname).to_owned();
    let postmaster = format!("postmaster@{domain}");
    let message = MessageBuilder::new()
        .from(("UwUMail".to_owned(), postmaster.clone()))
        .to(target.to_owned())
        .subject(subject)
        .date(Date::new(unix_now()))
        .message_id(format!("{}.forwarding@{domain}", crate::login::random_token()))
        .header("Auto-Submitted", mail_builder::headers::text::Text::new("auto-generated"))
        .text_body(body)
        .write_to_vec()
        .map_err(|_| ApiError::Internal)?;
    let signatures = match uwumail_smtp::dkim::ensure_domain_keys(web.store(), &domain).await {
        Ok(keys) => uwumail_smtp::dkim::sign(&message, &keys).unwrap_or_default(),
        Err(err) => {
            tracing::warn!(%err, %domain, "no DKIM keys for the forwarding confirmation");
            String::new()
        }
    };
    let mut signed = signatures.into_bytes();
    signed.extend_from_slice(&message);
    let recipient = NewQueueRecipient { address: target.to_owned(), notify_flags: 0, orcpt: None };
    web.store().enqueue(&postmaster, vec![recipient], &signed, Some(account.id), None, 7 * 24 * 3600).await?;
    tracing::info!(%login, %target, "sent a forwarding confirmation");
    Ok(())
}

async fn link(web: &Web, token: &str) -> ApiResult<(uwumail_store::ForwardTarget, Account)> {
    let invalid = || ApiError::Rule("linkInvalid", "this link is not valid (anymore)".into());
    if token.len() > 128 {
        return Err(invalid());
    }
    web.store().forward_link(token).await?.ok_or_else(invalid)
}

pub async fn show_link(State(web): State<Web>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    let (target, account) = link(&web, &token).await?;
    Ok(Json(json!({ "address": target.address, "from": account.login, "name": account.display_name })))
}

async fn answer(web: &Web, token: &str, confirm: bool) -> ApiResult<Json<Value>> {
    link(web, token).await?;
    let (target, account) = if confirm {
        web.store().confirm_forward_link(token).await?
    } else {
        web.store().decline_forward_link(token).await?
    };
    let event = SecurityEvent {
        kind: if confirm { "forwardingConfirmed" } else { "forwardingDeclined" }.into(),
        actor: String::new(),
        ip: String::new(),
        details: json!({ "address": target.address }),
    };
    if let Err(err) = web.store().record_security_event(account.id, event).await {
        tracing::error!(%err, "writing the security activity failed");
    }
    tracing::info!(login = %account.login, target = %target.address, confirm, "forwarding answered");
    Ok(Json(json!({ "address": target.address, "from": account.login })))
}

pub async fn confirm_link(State(web): State<Web>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    answer(&web, &token, true).await
}

pub async fn decline_link(State(web): State<Web>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    answer(&web, &token, false).await
}

fn vacation_json(vacation: &VacationResponse) -> Value {
    json!({
        "isEnabled": vacation.is_enabled,
        "fromDate": vacation.from_date,
        "toDate": vacation.to_date,
        "subject": vacation.subject,
        "textBody": vacation.text_body,
    })
}

pub async fn vacation(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(vacation_json(&web.store().vacation_response(session.account.id).await?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VacationUpdate {
    is_enabled: bool,
    from_date: Option<i64>,
    to_date: Option<i64>,
    subject: Option<String>,
    text_body: Option<String>,
}

pub async fn set_vacation(
    State(web): State<Web>,
    session: Session,
    Json(update): Json<VacationUpdate>,
) -> ApiResult<Json<Value>> {
    if let (Some(from), Some(to)) = (update.from_date, update.to_date)
        && to <= from
    {
        return Err(ApiError::Rule("vacationDates", "the end must be after the start".into()));
    }
    let text = update.text_body.map(|text| text.chars().take(10_000).collect::<String>());
    if update.is_enabled && text.as_deref().is_none_or(|text| text.trim().is_empty()) {
        return Err(ApiError::Rule("vacationText", "an away message needs a text".into()));
    }
    let vacation = VacationResponse {
        is_enabled: update.is_enabled,
        from_date: update.from_date,
        to_date: update.to_date,
        subject: update
            .subject
            .map(|subject| subject.chars().take(200).collect())
            .filter(|s: &String| !s.trim().is_empty()),
        text_body: text,
        // The portal edits plain text; an HTML version from a JMAP app would no longer match it.
        html_body: None,
    };
    web.store().set_vacation_response(session.account.id, vacation.clone()).await?;
    Ok(Json(vacation_json(&vacation)))
}

#[derive(Deserialize)]
pub struct ExternalForwarding {
    blocked: bool,
}

pub async fn set_external_forwarding(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(body): Json<ExternalForwarding>,
) -> ApiResult<StatusCode> {
    let account = web.store().account(&login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
    web.store().set_external_forwarding_blocked(account.id, body.blocked).await?;
    super::audit(&web, &session, "account.externalForwarding", &account.login, json!({ "blocked": body.blocked }))
        .await;
    Ok(StatusCode::NO_CONTENT)
}
