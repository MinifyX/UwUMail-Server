//! The setup assistant. On a fresh server a one-time code from the log unlocks creating the first
//! domain and admin; the checks and the test mail afterwards are ordinary admin requests, and
//! stay available under Server → Setup.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::{Extension, Json};
use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::servercheck::ServerCheck;
use uwumail_smtp::{Language, Submission, SubmissionRecipient};
use uwumail_store::{AuditEntry, NewAccount, Role, TestMessageStatus};

use super::check_password;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::session::Admin;

/// Letters and digits nobody mixes up.
const ALPHABET: &[u8; 31] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// A code like `k7mp-2xqa-9vtr`: about 59 bits, typed once from the server log.
pub fn new_setup_code() -> String {
    let mut code = String::new();
    while code.len() < 12 {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the system RNG failed");
        for byte in bytes {
            if byte < 248 && code.len() < 12 {
                code.push(ALPHABET[usize::from(byte % 31)] as char);
            }
        }
    }
    format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..])
}

fn normalized(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect()
}

impl Web {
    /// Opens the assistant when the server has no admin yet. Returns the code to print.
    pub async fn open_setup(&self) -> Option<String> {
        let counts = self.store().server_counts().await.ok()?;
        if counts.admins > 0 {
            return None;
        }
        let code = new_setup_code();
        *self.inner.setup_code.lock().expect("setup code poisoned") = Some(normalized(&code));
        Some(code)
    }

    fn setup_code_matches(&self, code: &str) -> bool {
        let expected = self.inner.setup_code.lock().expect("setup code poisoned").clone();
        let given = normalized(code);
        expected.is_some_and(|expected| {
            aws_lc_rs::constant_time::verify_slices_are_equal(expected.as_bytes(), given.as_bytes()).is_ok()
        })
    }

    fn close_setup(&self) {
        *self.inner.setup_code.lock().expect("setup code poisoned") = None;
    }
}

pub async fn status(State(web): State<Web>) -> ApiResult<Json<Value>> {
    let counts = web.store().server_counts().await?;
    let open = counts.admins == 0 && web.inner.setup_code.lock().expect("setup code poisoned").is_some();
    Ok(Json(json!({
        "open": open,
        "hostname": web.settings().hostname,
        "domains": web.store().domains().await?.into_iter().map(|domain| domain.name).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
}

async fn require_code(web: &Web, client: ClientInfo, code: &str) -> ApiResult<()> {
    if web.limiter().is_blocked(client.ip) {
        return Err(ApiError::TooManyAttempts);
    }
    if web.store().server_counts().await?.admins > 0 {
        web.close_setup();
        return Err(ApiError::Rule("setupDone", "this server already has an admin".into()));
    }
    if !web.setup_code_matches(code) {
        web.limiter().record_failure(client.ip);
        tracing::warn!(ip = %client.ip, "wrong setup code");
        return Err(ApiError::Rule("setupCodeInvalid", "the code is wrong".into()));
    }
    Ok(())
}

pub async fn verify_code(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    Json(body): Json<Code>,
) -> ApiResult<Json<Value>> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    require_code(&web, client, &body.code).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstAdmin {
    code: String,
    domain: String,
    local_part: String,
    #[serde(default)]
    name: String,
    password: String,
}

/// Creates the first domain and admin, then logs the admin in.
pub async fn complete(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Json(body): Json<FirstAdmin>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    require_code(&web, client, &body.code).await?;
    let domain = uwumail_store::normalize_domain(&body.domain)?;
    let address = format!("{}@{domain}", body.local_part.trim());
    let (local, _) = uwumail_store::normalize_address(&address)?;
    let login = format!("{local}@{domain}");
    check_password(&body.password, &login)?;

    if web.store().domain(&domain).await?.is_none() {
        web.store().create_domain(&domain).await?;
    }
    if let Err(err) = uwumail_smtp::dkim::ensure_domain_keys(web.store(), &domain).await {
        tracing::error!(%err, %domain, "creating DKIM keys failed");
        return Err(ApiError::Internal);
    }
    let account = web
        .store()
        .create_account(NewAccount {
            address: login.clone(),
            display_name: body.name.trim().to_owned(),
            password: Some(body.password.clone()),
            role: Role::Admin,
            quota_bytes: 0,
        })
        .await?;
    web.close_setup();
    let entry = AuditEntry {
        actor_id: Some(account.id),
        actor: account.login.clone(),
        action: "setup.complete".into(),
        target: domain.clone(),
        details: json!({ "admin": account.login }),
        ip: client.ip.to_string(),
    };
    if let Err(err) = web.store().record_audit(entry).await {
        tracing::error!(%err, "writing the change log failed");
    }
    tracing::info!(login = %account.login, %domain, "setup finished, the first admin exists");
    web.limiter().record_success(client.ip);
    super::auth::complete_login(&web, &account, client, &headers, "setup").await
}

#[derive(Deserialize, Default)]
pub struct CheckRequest {
    #[serde(default)]
    blocklists: bool,
}

pub async fn run_check(
    State(web): State<Web>,
    _admin: Admin,
    body: Option<Json<CheckRequest>>,
) -> ApiResult<Json<ServerCheck>> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let Some(dns) = web.dns() else {
        return Err(ApiError::Rule("dnsUnavailable", "the server has no working DNS resolver".into()));
    };
    let report = web.smtp().check_server(dns, request.blocklists).await;
    *web.inner.server_check.lock().expect("server check poisoned") = Some(report.clone());
    Ok(Json(report))
}

pub async fn last_check(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Option<ServerCheck>>> {
    Ok(Json(web.inner.server_check.lock().expect("server check poisoned").clone()))
}

#[derive(Deserialize, Default)]
pub struct TestMail {
    /// Another address of the admin's, to test sending out and receiving the reply.
    #[serde(default)]
    external: Option<String>,
}

pub async fn send_test_mail(
    State(web): State<Web>,
    Admin(session): Admin,
    body: Option<Json<TestMail>>,
) -> ApiResult<Json<Value>> {
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let account = &session.account;
    let domain = account.login.rsplit_once('@').map(|(_, domain)| domain.to_owned()).unwrap_or_default();
    let external = match request.external.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(address) => {
            let (local, host) = uwumail_store::normalize_address(address)?;
            Some(format!("{local}@{host}"))
        }
        None => None,
    };
    let preferences = web.store().preferences(account.id).await.unwrap_or_default();
    let language = match preferences.get("language").and_then(Value::as_str) {
        Some("en") => Language::En,
        Some("de") => Language::De,
        _ => web.smtp().tone().language,
    };
    let (subject, body) = match (language, external.is_some()) {
        (Language::De, true) => (
            "UwUMail: Test-Mail",
            format!(
                "Hallo!\n\nDiese Mail kommt von deinem neuen Server {}.\n\nAntworte bitte von {} aus auf diese Mail. \
                 Sobald die Antwort ankommt, weiß der Assistent, dass Mails von außen dich erreichen.\n",
                web.settings().hostname,
                external.as_deref().unwrap_or_default()
            ),
        ),
        (Language::De, false) => (
            "UwUMail: Test-Mail",
            format!(
                "Hallo!\n\nDiese Mail kommt von deinem neuen Server {}. Sie ist angekommen (=^･ω･^=)\n",
                web.settings().hostname
            ),
        ),
        (Language::En, true) => (
            "UwUMail: test message",
            format!(
                "Hello!\n\nThis message comes from your new server {}.\n\nPlease reply to it from {}. \
                 As soon as the reply arrives, the assistant knows mail from outside reaches you.\n",
                web.settings().hostname,
                external.as_deref().unwrap_or_default()
            ),
        ),
        (Language::En, false) => (
            "UwUMail: test message",
            format!(
                "Hello!\n\nThis message comes from your new server {}. It arrived (=^･ω･^=)\n",
                web.settings().hostname
            ),
        ),
    };
    let message_id = format!("{}.test@{domain}", crate::login::random_token()[..24].to_owned());
    let mut builder = MessageBuilder::new()
        .from((account.display_name.clone(), account.login.clone()))
        .subject(subject)
        .date(Date::new(unix_now()))
        .message_id(message_id.clone())
        .text_body(body);
    builder = match &external {
        Some(address) => builder.to(vec![account.login.clone(), address.clone()]),
        None => builder.to(account.login.clone()),
    };
    let raw = builder.write_to_vec().map_err(|_| ApiError::Internal)?;
    let mut recipients = vec![SubmissionRecipient::new(account.login.clone())];
    if let Some(address) = &external {
        recipients.push(SubmissionRecipient::new(address.clone()));
    }
    let submission = Submission {
        account: account.clone(),
        mail_from: account.login.clone(),
        recipients,
        raw,
        env_id: None,
        trace: None,
    };
    web.smtp().submit(submission).await.map_err(|err| {
        tracing::error!(?err, login = %account.login, "sending the test mail failed");
        ApiError::Rule("testMailFailed", "the test mail could not be sent".into())
    })?;
    Ok(Json(json!({ "messageId": message_id, "external": external })))
}

pub async fn test_mail_status(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(message_id): Path<String>,
) -> ApiResult<Json<TestMessageStatus>> {
    Ok(Json(web.store().test_message_status(session.account.id, &message_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_codes_are_readable() {
        let code = new_setup_code();
        assert_eq!(code.len(), 14);
        assert_eq!(normalized(&code.to_uppercase()).len(), 12);
        assert_eq!(normalized(" k7mp 2xqa-9VTR "), "k7mp2xqa9vtr");
    }
}
