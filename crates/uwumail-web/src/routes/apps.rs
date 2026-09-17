//! Setting up mail apps without typing server names: Thunderbird-style autoconfig, Outlook's
//! Autodiscover and configuration profiles for iPhone, iPad and Mac.

use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{AppScope, NewAppPassword};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::login::{random_bytes, random_token};
use crate::notices::{Notice, Origin, notify};
use crate::routes::security::{confirm_identity, origin};
use crate::session::Session;

pub const IMAP_PORT: u16 = 993;
pub const SUBMISSIONS_PORT: u16 = 465;
pub const SUBMISSION_PORT: u16 = 587;
/// A profile waits this long for its download; it holds a password.
const PROFILE_LIFETIME: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING_PROFILES: usize = 1000;
const MAX_AUTODISCOVER_BODY: usize = 16 * 1024;

pub struct PendingProfile {
    pub file: Vec<u8>,
    pub filename: String,
    pub created: Instant,
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

async fn hosted_domain(web: &Web, address: &str) -> ApiResult<Option<String>> {
    let Some((_, domain)) = address.trim().rsplit_once('@') else {
        return Ok(None);
    };
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let hosted = web.store().domains().await?.into_iter().any(|known| known.name == domain);
    Ok(hosted.then_some(domain))
}

#[derive(Deserialize)]
pub struct AutoconfigQuery {
    emailaddress: Option<String>,
}

/// `GET /mail/config-v1.1.xml` and `/.well-known/autoconfig/mail/config-v1.1.xml`, as Thunderbird,
/// K-9 / Thunderbird for Android and others ask for it.
pub async fn autoconfig(State(web): State<Web>, Query(query): Query<AutoconfigQuery>) -> ApiResult<Response> {
    let domain = match &query.emailaddress {
        Some(address) => match hosted_domain(&web, address).await? {
            Some(domain) => domain,
            None => return Ok(StatusCode::NOT_FOUND.into_response()),
        },
        None => "%EMAILDOMAIN%".to_owned(),
    };
    let host = xml(&web.settings().hostname);
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="{host}">
    <domain>{domain}</domain>
    <displayName>UwUMail</displayName>
    <displayShortName>UwUMail</displayShortName>
    <incomingServer type="imap">
      <hostname>{host}</hostname>
      <port>{IMAP_PORT}</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>{host}</hostname>
      <port>{SUBMISSIONS_PORT}</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
    <outgoingServer type="smtp">
      <hostname>{host}</hostname>
      <port>{SUBMISSION_PORT}</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
  </emailProvider>
</clientConfig>
"#,
        domain = xml(&domain)
    );
    Ok(([(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response())
}

fn autodiscover_error(code: u16, message: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response>
    <Error Time="00:00:00" Id="1">
      <ErrorCode>{code}</ErrorCode>
      <Message>{}</Message>
      <DebugData />
    </Error>
  </Response>
</Autodiscover>
"#,
        xml(message)
    );
    ([(header::CONTENT_TYPE, "text/xml; charset=utf-8")], body).into_response()
}

/// `POST /autodiscover/autodiscover.xml`: Outlook's "POX" request with the address in
/// `<EMailAddress>`. The answer only says where IMAP and SMTP are; it tells nobody whether a
/// mailbox exists.
pub async fn autodiscover(State(web): State<Web>, body: Bytes) -> ApiResult<Response> {
    if body.len() > MAX_AUTODISCOVER_BODY {
        return Ok(autodiscover_error(600, "Invalid Request"));
    }
    let request = String::from_utf8_lossy(&body);
    let address = request
        .split_once("<EMailAddress>")
        .and_then(|(_, rest)| rest.split_once("</EMailAddress>"))
        .map(|(address, _)| address.trim().to_owned());
    let Some(address) = address.filter(|address| address.contains('@') && address.len() <= 254) else {
        return Ok(autodiscover_error(600, "Invalid Request"));
    };
    if hosted_domain(&web, &address).await?.is_none() {
        return Ok(autodiscover_error(600, "Invalid Request"));
    }
    let (host, login) = (xml(&web.settings().hostname), xml(&address));
    let protocol = |kind: &str, port: u16, extra: &str| {
        format!(
            "      <Protocol>
        <Type>{kind}</Type>
        <Server>{host}</Server>
        <Port>{port}</Port>
        <DomainRequired>off</DomainRequired>
        <LoginName>{login}</LoginName>
        <SPA>off</SPA>
        <SSL>on</SSL>
        <AuthRequired>on</AuthRequired>{extra}
      </Protocol>
"
        )
    };
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a">
    <Account>
      <AccountType>email</AccountType>
      <Action>settings</Action>
{}{}    </Account>
  </Response>
</Autodiscover>
"#,
        protocol("IMAP", IMAP_PORT, ""),
        protocol("SMTP", SUBMISSIONS_PORT, "\n        <UsePOPAuth>on</UsePOPAuth>\n        <SMTPLast>off</SMTPLast>"),
    );
    Ok(([(header::CONTENT_TYPE, "text/xml; charset=utf-8")], body).into_response())
}

fn uuid() -> String {
    let mut bytes = random_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes[..16].iter().map(|b| format!("{b:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// The reverse-DNS identifier Apple wants, from the host name and the login.
fn identifier(hostname: &str, login: &str) -> String {
    let mut parts: Vec<String> = hostname.split('.').rev().map(str::to_owned).collect();
    parts.push("uwumail".into());
    parts.push(login.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect());
    parts.join(".")
}

/// A configuration profile with mail, calendars and contacts, the app password inside.
pub fn apple_profile(hostname: &str, login: &str, display_name: &str, secret: &str) -> String {
    let (host, address, name, secret) = (xml(hostname), xml(login), xml(display_name), xml(secret));
    let name = if name.is_empty() { address.clone() } else { name };
    let id = xml(&identifier(hostname, login));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>EmailAccountDescription</key>
      <string>{address}</string>
      <key>EmailAccountName</key>
      <string>{name}</string>
      <key>EmailAccountType</key>
      <string>EmailTypeIMAP</string>
      <key>EmailAddress</key>
      <string>{address}</string>
      <key>IncomingMailServerAuthentication</key>
      <string>EmailAuthPassword</string>
      <key>IncomingMailServerHostName</key>
      <string>{host}</string>
      <key>IncomingMailServerPortNumber</key>
      <integer>{IMAP_PORT}</integer>
      <key>IncomingMailServerUseSSL</key>
      <true/>
      <key>IncomingMailServerUsername</key>
      <string>{address}</string>
      <key>IncomingPassword</key>
      <string>{secret}</string>
      <key>OutgoingMailServerAuthentication</key>
      <string>EmailAuthPassword</string>
      <key>OutgoingMailServerHostName</key>
      <string>{host}</string>
      <key>OutgoingMailServerPortNumber</key>
      <integer>{SUBMISSIONS_PORT}</integer>
      <key>OutgoingMailServerUseSSL</key>
      <true/>
      <key>OutgoingMailServerUsername</key>
      <string>{address}</string>
      <key>OutgoingPasswordSameAsIncomingPassword</key>
      <true/>
      <key>PayloadDescription</key>
      <string>Mail account {address}</string>
      <key>PayloadDisplayName</key>
      <string>Mail ({address})</string>
      <key>PayloadIdentifier</key>
      <string>{id}.mail</string>
      <key>PayloadType</key>
      <string>com.apple.mail.managed</string>
      <key>PayloadUUID</key>
      <string>{mail_uuid}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
      <key>PreventAppSheet</key>
      <false/>
      <key>PreventMove</key>
      <false/>
    </dict>
    <dict>
      <key>CalDAVAccountDescription</key>
      <string>{address}</string>
      <key>CalDAVHostName</key>
      <string>{host}</string>
      <key>CalDAVPort</key>
      <integer>443</integer>
      <key>CalDAVPrincipalURL</key>
      <string>/dav/principals/{address}/</string>
      <key>CalDAVUseSSL</key>
      <true/>
      <key>CalDAVUsername</key>
      <string>{address}</string>
      <key>CalDAVPassword</key>
      <string>{secret}</string>
      <key>PayloadDescription</key>
      <string>Calendars of {address}</string>
      <key>PayloadDisplayName</key>
      <string>Calendars ({address})</string>
      <key>PayloadIdentifier</key>
      <string>{id}.caldav</string>
      <key>PayloadType</key>
      <string>com.apple.caldav.account</string>
      <key>PayloadUUID</key>
      <string>{caldav_uuid}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
    <dict>
      <key>CardDAVAccountDescription</key>
      <string>{address}</string>
      <key>CardDAVHostName</key>
      <string>{host}</string>
      <key>CardDAVPort</key>
      <integer>443</integer>
      <key>CardDAVPrincipalURL</key>
      <string>/dav/principals/{address}/</string>
      <key>CardDAVUseSSL</key>
      <true/>
      <key>CardDAVUsername</key>
      <string>{address}</string>
      <key>CardDAVPassword</key>
      <string>{secret}</string>
      <key>PayloadDescription</key>
      <string>Contacts of {address}</string>
      <key>PayloadDisplayName</key>
      <string>Contacts ({address})</string>
      <key>PayloadIdentifier</key>
      <string>{id}.carddav</string>
      <key>PayloadType</key>
      <string>com.apple.carddav.account</string>
      <key>PayloadUUID</key>
      <string>{carddav_uuid}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDescription</key>
  <string>Sets up {address} from {host} with its own app password.</string>
  <key>PayloadDisplayName</key>
  <string>UwUMail ({address})</string>
  <key>PayloadIdentifier</key>
  <string>{id}</string>
  <key>PayloadOrganization</key>
  <string>UwUMail</string>
  <key>PayloadRemovalDisallowed</key>
  <false/>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadUUID</key>
  <string>{profile_uuid}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
</dict>
</plist>
"#,
        mail_uuid = uuid(),
        caldav_uuid = uuid(),
        carddav_uuid = uuid(),
        profile_uuid = uuid(),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppleProfileRequest {
    /// Shown in the list of app passwords, like "iPhone".
    device: String,
    #[serde(default)]
    password: Option<String>,
}

/// Makes an app password and a profile with it, and returns a one-time link to download it.
pub async fn create_apple_profile(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<AppleProfileRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    confirm_identity(&web, &session, request.password.as_deref()).await?;
    let device = request.device.trim();
    if device.is_empty() || device.chars().count() > 60 {
        return Err(ApiError::Invalid("the device name is empty or too long".into()));
    }
    let created = web
        .store()
        .create_app_password(
            session.account.id,
            NewAppPassword {
                name: device.to_owned(),
                scopes: vec![AppScope::Mail, AppScope::Smtp, AppScope::Dav],
                expires_at: None,
            },
        )
        .await?;
    let (actor, ip) = origin(&session);
    let notice = Notice::AppPasswordCreated { name: created.app_password.name.clone() };
    notify(&web, &session.account, notice, Origin { actor: &actor, ip: &ip }).await;

    let account = &session.account;
    let file = apple_profile(&web.settings().hostname, &account.login, &account.display_name, &created.secret);
    let token = random_token();
    web.keep_profile(
        token.clone(),
        PendingProfile {
            file: file.into_bytes(),
            filename: format!("{}.mobileconfig", account.login.replace(['@', '.'], "-")),
            created: Instant::now(),
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "appPassword": created.app_password, "url": format!("/api/apple-profiles/{token}") })),
    ))
}

/// `GET /api/apple-profiles/{token}`: the profile, once, within ten minutes.
pub async fn download_apple_profile(State(web): State<Web>, Path(token): Path<String>) -> Response {
    let Some(profile) = web.take_profile(&token).filter(|profile| profile.created.elapsed() < PROFILE_LIFETIME) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let disposition = format!("attachment; filename=\"{}\"", profile.filename);
    let mut response = (StatusCode::OK, profile.file).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-apple-aspen-config"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

impl Web {
    fn keep_profile(&self, token: String, profile: PendingProfile) {
        let mut pending = self.inner.apple_profiles.lock().expect("profiles poisoned");
        pending.retain(|_, profile| profile.created.elapsed() < PROFILE_LIFETIME);
        if pending.len() >= MAX_PENDING_PROFILES {
            return;
        }
        pending.insert(token, profile);
    }

    fn take_profile(&self, token: &str) -> Option<PendingProfile> {
        self.inner.apple_profiles.lock().expect("profiles poisoned").remove(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_escape_everything_they_carry() {
        let profile = apple_profile("mail.example.de", "mini@example.de", "Mini <&> \"Katze\"", "abc&def");
        assert!(profile.contains("<string>Mini &lt;&amp;&gt; &quot;Katze&quot;</string>"));
        assert!(profile.contains("<string>abc&amp;def</string>"));
        assert!(profile.contains("<string>de.example.mail.uwumail.mini-example-de</string>"));
        assert!(profile.contains("<integer>993</integer>"));
        let uuid = uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
    }
}
