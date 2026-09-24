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
    // Domains added on the command line fill in the form; nobody else needs to see them.
    let domains = match open {
        true => web.store().domains().await?.into_iter().map(|domain| domain.name).collect(),
        false => Vec::new(),
    };
    Ok(Json(json!({ "open": open, "hostname": web.settings().hostname, "domains": domains })))
}

#[derive(Deserialize)]
pub struct Code {
    code: String,
}

/// How wrong setup codes are counted in the login throttle: as tries at a login of their own.
const SETUP_CODE: &str = "setup code";

async fn require_code(web: &Web, client: ClientInfo, code: &str) -> ApiResult<()> {
    if web.limiter().is_blocked(client.ip) {
        return Err(ApiError::TooManyAttempts);
    }
    if web.store().server_counts().await?.admins > 0 {
        web.close_setup();
        return Err(ApiError::Rule("setupDone", "this server already has an admin".into()));
    }
    if !web.setup_code_matches(code) {
        web.limiter().record_failure(client.ip, SETUP_CODE);
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

/// A backup server to look at, as the assistant asks for it.
///
/// A machine standing in for one that died has no key on that backup server, and no way to put one
/// there — the machine that had it is gone. So a private key can be pasted here. It is used for
/// this one look, and only kept if a restore actually follows.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupServer {
    code: String,
    host: String,
    #[serde(default = "twenty_two")]
    port: u16,
    user: String,
    path: String,
    /// `password` or `key`.
    method: String,
    #[serde(default)]
    password: Option<String>,
    /// An OpenSSH private key in full, for a backup server that only takes keys.
    #[serde(default)]
    private_key: Option<String>,
    /// Needed when the repository is encrypted; without it the look only says that it is.
    #[serde(default)]
    recovery_key: Option<String>,
    /// Only for the restore: which snapshot, and `latest` for the newest.
    #[serde(default)]
    snapshot: String,
}

fn twenty_two() -> u16 {
    22
}

impl BackupServer {
    fn target(&self) -> ApiResult<uwumail_backup::Target> {
        let login = match self.method.as_str() {
            "key" => uwumail_backup::Login::Key {
                private_key: self
                    .private_key
                    .clone()
                    .filter(|key| key.contains("PRIVATE KEY"))
                    .ok_or_else(|| ApiError::Invalid("paste the private key of the backup server".into()))?,
            },
            "password" => uwumail_backup::Login::Password {
                password: self
                    .password
                    .clone()
                    .filter(|password| !password.is_empty())
                    .ok_or_else(|| ApiError::Invalid("the password is missing".into()))?,
            },
            other => return Err(ApiError::Invalid(format!("unknown login: {other}"))),
        };
        Ok(uwumail_backup::Target {
            host: self.host.trim().to_owned(),
            port: self.port,
            user: self.user.trim().to_owned(),
            path: self.path.trim().to_owned(),
            login,
            host_key: None,
        })
    }
}

/// What is on a backup server, before anything is decided. Nothing is saved by this.
pub async fn backup_look(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    Json(body): Json<BackupServer>,
) -> ApiResult<Json<Value>> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    require_code(&web, client, &body.code).await?;
    let look = uwumail_backup::Backups::look_at(&body.target()?, body.recovery_key.as_deref())
        .await
        .map_err(super::backups::api_error)?;
    Ok(Json(json!({
        "hostKey": look.host_key,
        "encrypted": look.encrypted,
        "snapshots": look
            .snapshots
            .into_iter()
            .map(|(name, manifest)| {
                json!({
                    "name": name,
                    "createdAt": manifest.created_at,
                    "hostname": manifest.hostname,
                    "version": manifest.version,
                    "mails": manifest.blobs.len(),
                    "size": manifest.database_size + manifest.blobs_size,
                })
            })
            .collect::<Vec<_>>(),
    })))
}

/// Puts a backup back onto a server that has not been set up yet.
///
/// The backup server is saved first, because that is what the restore opens — and everything saved
/// here is replaced by the snapshot's own settings a minute later anyway, which are then switched
/// off. The assistant stays open until an admin exists, so a restore that fails leaves the machine
/// exactly where it was.
pub async fn backup_restore(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    Json(body): Json<BackupServer>,
) -> ApiResult<Json<Value>> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    require_code(&web, client, &body.code).await?;
    let backups = web.backups().ok_or_else(|| ApiError::NotFound("backups on this server".into()))?;
    if !backups.can_restore() {
        return Err(ApiError::Rule("backupFailed", "this server cannot restore into itself".into()));
    }
    let settings = uwumail_backup::BackupSettings {
        enabled: false,
        target: Some(body.target()?),
        key: body.recovery_key.clone().filter(|key| !key.trim().is_empty()),
        ..uwumail_backup::BackupSettings::default()
    };
    backups.save_settings(&settings).await.map_err(super::backups::api_error)?;
    let snapshot = match body.snapshot.trim() {
        "" => "latest",
        name => name,
    };
    backups.start_restore(snapshot, false, "setup").await.map_err(super::backups::api_error)?;
    tracing::warn!(%snapshot, "the setup assistant is putting a backup back");
    Ok(Json(json!({ "started": true })))
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
            protocols: None,
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
    web.limiter().record_success(client.ip, SETUP_CODE);
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
    let language = crate::notices::language(&web, &preferences);
    let brand = web.smtp().brand();
    let face = if brand.mascot { " (=^･ω･^=)" } else { "" };
    let hostname = &web.settings().hostname;
    let reply_from = external.as_deref().unwrap_or_default();
    // Subject, greeting, where it comes from, asking for a reply, and that it arrived.
    let (label, hello, from, reply, arrived) = match language {
        Language::De => (
            "Test-Mail",
            "Hallo!",
            format!("Diese Mail kommt von deinem neuen Server {hostname}."),
            format!(
                "Antworte bitte von {reply_from} aus auf diese Mail. \
                 Sobald die Antwort ankommt, weiß der Assistent, dass Mails von außen dich erreichen."
            ),
            "Sie ist angekommen",
        ),
        Language::En => (
            "test message",
            "Hello!",
            format!("This message comes from your new server {hostname}."),
            format!(
                "Please reply to it from {reply_from}. \
                 As soon as the reply arrives, the assistant knows mail from outside reaches you."
            ),
            "It arrived",
        ),
        Language::Fr => (
            "message de test",
            "Bonjour !",
            format!("Ce message vient de votre nouveau serveur {hostname}."),
            format!(
                "Veuillez y répondre depuis {reply_from}. \
                 Dès que la réponse arrive, l'assistant sait que les e-mails de l'extérieur vous parviennent."
            ),
            "Il est bien arrivé",
        ),
        Language::Nl => (
            "testbericht",
            "Hallo!",
            format!("Dit bericht komt van je nieuwe server {hostname}."),
            format!(
                "Beantwoord het vanaf {reply_from}. \
                 Zodra het antwoord binnenkomt, weet de assistent dat mail van buiten je bereikt."
            ),
            "Het is aangekomen",
        ),
        Language::Ja => (
            "テストメール",
            "こんにちは！",
            format!("このメールは新しいサーバー {hostname} から送られています。"),
            format!(
                "{reply_from} からこのメールに返信してください。\
                 返信が届いた時点で、外部からのメールが届くことをアシスタントが確認できます。"
            ),
            "無事に届きました",
        ),
        Language::Zh => (
            "测试邮件",
            "你好！",
            format!("这封邮件来自你的新服务器 {hostname}。"),
            format!("请从 {reply_from} 回复这封邮件。回复一到，助手就知道外部邮件能送达你了。"),
            "已经送达",
        ),
    };
    let subject = format!("{}: {label}", brand.name());
    let body = match &external {
        Some(_) => format!("{hello}\n\n{from}\n\n{reply}\n"),
        None => format!("{hello}\n\n{from} {arrived}{face}\n"),
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
