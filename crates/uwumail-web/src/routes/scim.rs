//! SCIM 2.0 (RFC 7643, RFC 7644) at `/scim/v2`: how a paired UwUAuth tells this server who has a
//! mailbox (docs/uwuauth.md).
//!
//! - **People** are mailboxes: a new one gets an account without a password that logs in with
//!   UwUAuth (`auth_source` `oidc`), on a domain of this server only. `active: false` locks the
//!   account; `active: true` unlocks only an account SCIM locked itself, never one an admin did.
//!   `DELETE` moves it to the trash, where an admin can still restore it. The role `admin`
//!   makes an admin. The address never changes this way.
//! - Only such **managed** accounts (`oidc`, no password) follow the provider. An account that
//!   was here before (a 409 on create, then a `PATCH` of `externalId`) keeps the `externalId`
//!   but nothing else: no lock, role, name or trash, and no login link — that waits for the
//!   first OpenID Connect login, which asks for a verified email.
//! - **Groups** are kept so the provider gets proper answers, but mean nothing for mail.
//!
//! The token comes from the pairing; only its SHA-256 is kept and compared in constant time, and
//! refused tokens count against the client's network in a budget of their own.

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{
    Account, AccountUpdate, AuditEntry, NewAccount, Role, SCIM_MAX_MEMBERS, ScimGroup, ScimGroupFilter,
    ScimGroupUpdate, ScimUser, ScimUserFilter, StoreError,
};

use super::oauth::Budget;
use super::uwuauth::{Pairing, pairing, token_hash};
use crate::Web;
use crate::session::{client, same};

const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
/// The most one page lists.
const MAX_RESULTS: usize = 200;
/// The largest request body.
pub(crate) const MAX_BODY: usize = 1024 * 1024;
/// Who SCIM is in the change log.
const ACTOR: &str = "scim";
/// Refused SCIM tokens per network before the endpoints wait; apart from wrong passwords.
const REFUSED_TOKENS: Budget = Budget { name: "scim", max: 20, window: 15 * 60 };
/// The largest `startIndex` taken; beyond it a page is empty anyway.
const MAX_START: usize = 1 << 40;

// ── Answers ───────────────────────────────────────────────

/// A SCIM error: status, `scimType` and a detail.
#[derive(Debug)]
pub struct ScimError {
    status: StatusCode,
    kind: Option<&'static str>,
    detail: String,
}

impl ScimError {
    fn new(status: StatusCode, kind: Option<&'static str>, detail: impl Into<String>) -> Self {
        ScimError { status, kind, detail: detail.into() }
    }

    fn bad(kind: &'static str, detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Some(kind), detail)
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, None, "No such resource.")
    }

    fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, Some("uniqueness"), detail)
    }
}

impl From<StoreError> for ScimError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound(_) => Self::not_found(),
            StoreError::Invalid(detail) => Self::bad("invalidValue", detail),
            StoreError::Conflict(what) => Self::conflict(format!("Already taken: {what}.")),
            StoreError::Rule { code: "lastAdmin", message } => {
                Self::new(StatusCode::CONFLICT, Some("mutability"), message)
            }
            StoreError::Rule { message, .. } => Self::bad("invalidValue", message),
            StoreError::Busy => Self::new(StatusCode::SERVICE_UNAVAILABLE, None, "The server is busy, try again."),
            other => {
                tracing::error!(error = %other, "a SCIM request failed");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, None, "Something went wrong on the server.")
            }
        }
    }
}

impl IntoResponse for ScimError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "schemas": [ERROR_SCHEMA],
            "status": self.status.as_u16().to_string(),
            "detail": self.detail,
        });
        if let Some(kind) = self.kind {
            body["scimType"] = kind.into();
        }
        scim(self.status, body)
    }
}

type ScimResult<T> = Result<T, ScimError>;

fn scim(status: StatusCode, body: Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/scim+json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn created(body: Value, location: &str) -> Response {
    let mut response = scim(StatusCode::CREATED, body);
    if let Ok(location) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

fn list(resources: Vec<Value>, total: usize, start: usize) -> Response {
    scim(
        StatusCode::OK,
        json!({
            "schemas": [LIST_SCHEMA],
            "totalResults": total,
            "startIndex": start,
            "itemsPerPage": resources.len(),
            "Resources": resources,
        }),
    )
}

fn body_json(body: &Bytes) -> ScimResult<Value> {
    serde_json::from_slice(body).map_err(|_| ScimError::bad("invalidSyntax", "The body is not JSON."))
}

fn base(web: &Web) -> String {
    format!("https://{}/scim/v2", web.settings().hostname)
}

/// A Unix time as RFC 3339 in UTC, for `meta`.
fn format_utc(timestamp: i64) -> String {
    let (days, seconds) = (timestamp.div_euclid(86_400), timestamp.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

// ── The token ─────────────────────────────────────────────

/// A request with the SCIM token of the pairing in effect.
pub struct Scim {
    pairing: Pairing,
}

impl FromRequestParts<Web> for Scim {
    type Rejection = ScimError;

    async fn from_request_parts(parts: &mut Parts, web: &Web) -> Result<Self, Self::Rejection> {
        let ip = client(parts).ip;
        if !web.oauth_attempt(REFUSED_TOKENS, ip, false) {
            return Err(ScimError::new(StatusCode::TOO_MANY_REQUESTS, None, "Too many refused tokens. Wait a while."));
        }
        let presented = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")))
            .map(str::trim)
            .unwrap_or_default();
        let pairing = pairing(web).await.map_err(|_| {
            ScimError::new(StatusCode::INTERNAL_SERVER_ERROR, None, "Something went wrong on the server.")
        })?;
        match pairing {
            Some(pairing) if !presented.is_empty() && same(&token_hash(presented), &pairing.scim_token_hash) => {
                Ok(Scim { pairing })
            }
            _ => {
                web.oauth_attempt(REFUSED_TOKENS, ip, true);
                tracing::info!(%ip, "a SCIM request with a wrong token");
                Err(ScimError::new(StatusCode::UNAUTHORIZED, None, "The SCIM token is missing or wrong."))
            }
        }
    }
}

// ── What the server can do ────────────────────────────────

pub async fn service_provider_config(State(web): State<Web>, _scim: Scim) -> Response {
    scim(
        StatusCode::OK,
        json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
            "patch": { "supported": true },
            "bulk": { "supported": false, "maxOperations": 0, "maxPayloadSize": 0 },
            "filter": { "supported": true, "maxResults": MAX_RESULTS },
            "changePassword": { "supported": false },
            "sort": { "supported": false },
            "etag": { "supported": false },
            "authenticationSchemes": [{
                "type": "oauthbearertoken",
                "name": "OAuth Bearer Token",
                "description": "The SCIM token from the pairing with UwUAuth",
                "primary": true,
            }],
            "meta": {
                "resourceType": "ServiceProviderConfig",
                "location": format!("{}/ServiceProviderConfig", base(&web)),
            },
        }),
    )
}

pub async fn resource_types(State(web): State<Web>, _scim: Scim) -> Response {
    let base = base(&web);
    let kind = |name: &str, endpoint: &str, schema: &str| {
        json!({
            "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ResourceType"],
            "id": name,
            "name": name,
            "endpoint": endpoint,
            "schema": schema,
            "meta": { "resourceType": "ResourceType", "location": format!("{base}/ResourceTypes/{name}") },
        })
    };
    list(vec![kind("User", "/Users", USER_SCHEMA), kind("Group", "/Groups", GROUP_SCHEMA)], 2, 1)
}

pub async fn schemas(_scim: Scim) -> Response {
    let attribute = |name: &str, kind: &str, mutability: &str, required: bool| {
        json!({
            "name": name, "type": kind, "multiValued": false, "required": required,
            "caseExact": false, "mutability": mutability, "returned": "default", "uniqueness": "none",
        })
    };
    let user = json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:Schema"],
        "id": USER_SCHEMA,
        "name": "User",
        "attributes": [
            attribute("userName", "string", "immutable", true),
            attribute("displayName", "string", "readWrite", false),
            attribute("active", "boolean", "readWrite", false),
            attribute("externalId", "string", "readWrite", false),
            { "name": "roles", "type": "complex", "multiValued": true, "required": false, "mutability": "readWrite" },
        ],
    });
    let group = json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:Schema"],
        "id": GROUP_SCHEMA,
        "name": "Group",
        "attributes": [
            attribute("displayName", "string", "readWrite", true),
            attribute("externalId", "string", "readWrite", false),
            { "name": "members", "type": "complex", "multiValued": true, "required": false, "mutability": "readWrite" },
        ],
    });
    list(vec![user, group], 2, 1)
}

// ── Filters and pages ─────────────────────────────────────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    start_index: Option<usize>,
    #[serde(default)]
    count: Option<usize>,
}

impl ListQuery {
    /// The 1-based start, and how many to show.
    fn window(&self) -> (usize, usize) {
        (self.start_index.unwrap_or(1).clamp(1, MAX_START), self.count.unwrap_or(MAX_RESULTS).min(MAX_RESULTS))
    }

    fn filter(&self) -> ScimResult<Option<(String, String)>> {
        self.filter.as_deref().filter(|filter| !filter.trim().is_empty()).map(parse_filter).transpose()
    }
}

/// `attribute eq "value"`, the one kind of filter providers send for a lookup. The attribute in
/// lower case.
fn parse_filter(filter: &str) -> ScimResult<(String, String)> {
    let invalid = || ScimError::bad("invalidFilter", "Only filters like userName eq \"a@example.com\" are supported.");
    let (attribute, rest) = filter.trim().split_once(char::is_whitespace).ok_or_else(invalid)?;
    let (operator, value) = rest.trim_start().split_once(char::is_whitespace).ok_or_else(invalid)?;
    if !operator.eq_ignore_ascii_case("eq") {
        return Err(invalid());
    }
    let value: String = serde_json::from_str(value.trim()).map_err(|_| invalid())?;
    Ok((attribute.to_ascii_lowercase(), value))
}

// ── People ────────────────────────────────────────────────

fn user_json(user: &ScimUser, base: &str) -> Value {
    let account = &user.account;
    let mut roles = vec![json!({ "value": "user" })];
    if account.role == Role::Admin {
        roles.insert(0, json!({ "value": "admin", "primary": true }));
    }
    json!({
        "schemas": [USER_SCHEMA],
        "id": user.id,
        "externalId": user.external_id,
        "userName": account.login,
        "displayName": account.display_name,
        "active": !account.disabled,
        "emails": [{ "value": account.login, "primary": true, "type": "work" }],
        "roles": roles,
        "meta": {
            "resourceType": "User",
            "created": format_utc(account.created_at),
            "lastModified": format_utc(user.updated.max(account.created_at)),
            "location": format!("{base}/Users/{}", user.id),
        },
    })
}

pub async fn list_users(State(web): State<Web>, _scim: Scim, Query(query): Query<ListQuery>) -> ScimResult<Response> {
    let filter = match query.filter()? {
        None => ScimUserFilter::All,
        Some((attribute, value)) => match attribute.as_str() {
            "username" | "emails" | "emails.value" => ScimUserFilter::UserName(value),
            "externalid" => ScimUserFilter::ExternalId(value),
            "id" => ScimUserFilter::Id(value),
            _ => return Err(ScimError::bad("invalidFilter", "Users are filtered by userName, externalId or id.")),
        },
    };
    let (start, count) = query.window();
    let (total, users) = web.store().scim_users(filter, start - 1, count).await?;
    let base = base(&web);
    Ok(list(users.iter().map(|user| user_json(user, &base)).collect(), total, start))
}

async fn user(web: &Web, id: &str) -> ScimResult<ScimUser> {
    web.store().scim_user(id, false).await?.ok_or_else(ScimError::not_found)
}

pub async fn get_user(State(web): State<Web>, _scim: Scim, Path(id): Path<String>) -> ScimResult<Response> {
    let user = user(&web, &id).await?;
    Ok(scim(StatusCode::OK, user_json(&user, &base(&web))))
}

/// The address a user resource names: `userName`, or its primary (or first) email.
fn user_name(body: &Value) -> Option<String> {
    body["userName"].as_str().map(str::to_owned).or_else(|| {
        let emails = body["emails"].as_array()?;
        let primary = emails.iter().find(|email| email["primary"] == true).or_else(|| emails.first())?;
        primary["value"].as_str().map(str::to_owned)
    })
}

/// An address in its normal form, if it is one.
fn address(name: &str) -> Option<String> {
    uwumail_store::normalize_address(name.trim()).ok().map(|(local, domain)| format!("{local}@{domain}"))
}

/// `true`, `false`, or the strings some providers send for them.
fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) if text.eq_ignore_ascii_case("true") => Some(true),
        Value::String(text) if text.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn clean_name(name: &str) -> String {
    name.trim().chars().filter(|c| !c.is_control()).take(100).collect()
}

/// A name for the account from a user resource: `displayName`, else `name.formatted`.
fn display_name(body: &Value) -> Option<String> {
    body["displayName"].as_str().or_else(|| body["name"]["formatted"].as_str()).map(clean_name)
}

fn text(value: &Value, most: usize) -> Option<String> {
    value.as_str().map(|text| text.trim().chars().take(most).collect::<String>()).filter(|text| !text.is_empty())
}

/// Whether a `roles` value names the admin role: `[{ "value": "admin" }]`, or plain strings.
fn names_admin(value: &Value) -> Option<bool> {
    let roles = match value {
        Value::Array(roles) => roles.as_slice(),
        Value::Null => return None,
        other => std::slice::from_ref(other),
    };
    Some(roles.iter().any(|role| {
        role["value"].as_str().or_else(|| role.as_str()).is_some_and(|name| name.trim().eq_ignore_ascii_case("admin"))
    }))
}

async fn log(web: &Web, action: &str, target: &str, details: Value) {
    let entry = AuditEntry {
        actor_id: None,
        actor: ACTOR.into(),
        action: action.into(),
        target: target.to_owned(),
        details,
        ip: String::new(),
    };
    if let Err(err) = web.store().record_audit(entry).await {
        tracing::error!(%err, "writing the change log failed");
    }
}

/// Whether the provider runs the account: SCIM made it (`oidc`, no password). Anything else was
/// here before and only gets its `externalId` noted.
async fn managed(web: &Web, account: &Account) -> ScimResult<bool> {
    Ok(!account.is_service()
        && web.store().auth_source(account.id).await? == "oidc"
        && !web.store().has_password(account.id).await?)
}

/// Notes the provider's id for logging in: the OpenID Connect `sub` is the SCIM `externalId`.
/// Only for managed accounts; see [`managed`].
async fn link_login(web: &Web, pairing: &Pairing, account: &Account, external_id: Option<&str>) -> ScimResult<()> {
    let Some(subject) = external_id else { return Ok(()) };
    if !web.store().link_external_identity(account.id, &pairing.issuer, subject, &account.login).await? {
        tracing::warn!(login = %account.login, "SCIM: the account is linked to another login at UwUAuth already");
    }
    Ok(())
}

pub async fn create_user(State(web): State<Web>, scim_auth: Scim, body: Bytes) -> ScimResult<Response> {
    let body = body_json(&body)?;
    let address = user_name(&body)
        .and_then(|name| address(&name))
        .ok_or_else(|| ScimError::bad("invalidValue", "userName has to be an email address."))?;
    let domain = address.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
    if !super::external_login::takes_people(&web, domain).await {
        return Err(ScimError::bad("invalidValue", format!("{domain} is not a mail domain of this server.")));
    }
    if web.store().account(&address).await?.is_some() {
        return Err(ScimError::conflict("There is an account with this userName already."));
    }
    let external = text(&body["externalId"], 255);
    if let Some(external) = &external {
        let (found, _) = web.store().scim_users(ScimUserFilter::ExternalId(external.clone()), 0, 1).await?;
        if found > 0 {
            return Err(ScimError::conflict("There is an account with this externalId already."));
        }
    }
    let admin = names_admin(&body["roles"]).unwrap_or(false);
    let name = display_name(&body).unwrap_or_default();
    let disabled = boolean(&body["active"]) == Some(false);
    let user = web
        .store()
        .scim_create_account(
            NewAccount {
                address: address.clone(),
                display_name: name,
                password: None,
                role: if admin { Role::Admin } else { Role::User },
                quota_bytes: 0,
                protocols: None,
            },
            external.clone(),
            disabled,
        )
        .await?;
    let account = &user.account;
    link_login(&web, &scim_auth.pairing, account, external.as_deref()).await?;
    log(&web, "account.create", &account.login, json!({ "via": ACTOR, "admin": admin })).await;
    tracing::info!(login = %account.login, admin, "SCIM: account created");
    let base = base(&web);
    Ok(created(user_json(&user, &base), &format!("{base}/Users/{}", user.id)))
}

/// What a `PATCH` or `PUT` changes of a person. `None`: left as it is.
#[derive(Default)]
struct PersonChange {
    active: Option<bool>,
    display_name: Option<String>,
    external_id: Option<Option<String>>,
    admin: Option<bool>,
}

/// The address named in a change, which may not be another one.
fn same_address(user: &ScimUser, value: &Value) -> ScimResult<()> {
    let named = match value {
        Value::String(name) => address(name),
        Value::Array(_) => user_name(&json!({ "emails": value })).and_then(|name| address(&name)),
        _ => None,
    };
    match named {
        Some(named) if named != user.account.login => {
            Err(ScimError::bad("mutability", "The address of a mailbox cannot change through SCIM."))
        }
        _ => Ok(()),
    }
}

fn change_of(user: &ScimUser, change: &mut PersonChange, path: &str, value: &Value, remove: bool) -> ScimResult<()> {
    match path.to_ascii_lowercase().as_str() {
        "active" if !remove => {
            change.active =
                Some(boolean(value).ok_or_else(|| ScimError::bad("invalidValue", "active is true or false."))?);
        }
        "displayname" | "name.formatted" => {
            change.display_name =
                Some(if remove { String::new() } else { value.as_str().map(clean_name).unwrap_or_default() })
        }
        "externalid" => change.external_id = Some(if remove { None } else { text(value, 255) }),
        "roles" => change.admin = Some(!remove && names_admin(value).unwrap_or(false)),
        "username" | "emails" | "emails.value" if !remove => same_address(user, value)?,
        "username" | "emails" => return Err(ScimError::bad("mutability", "The address cannot be removed.")),
        // Everything else a provider may send is not kept here.
        _ => {}
    }
    Ok(())
}

/// Puts a change into effect.
async fn apply(web: &Web, pairing: &Pairing, user: ScimUser, change: PersonChange) -> ScimResult<ScimUser> {
    let account = &user.account;
    if !managed(web, account).await? {
        // Taken over after a 409: the provider's id is noted, and that is all. The login link
        // comes with the first OpenID Connect login, which needs a verified email.
        if let Some(external) = change.external_id.filter(|external| *external != user.external_id) {
            web.store().scim_set_external_id(&user.id, external).await?;
        }
        web.store().scim_touch(&user.id, None).await?;
        return self::user(web, &user.id).await;
    }
    let mut update = AccountUpdate::default();
    let mut flag = None;
    match change.active {
        Some(false) if !account.disabled => {
            update.disabled = Some(true);
            flag = Some(true);
        }
        // Only what SCIM locked, SCIM unlocks: an account an admin locked stays locked.
        Some(true) if account.disabled && user.disabled_by_scim => {
            update.disabled = Some(false);
            flag = Some(false);
        }
        _ => {}
    }
    if let Some(name) = change.display_name.filter(|name| *name != account.display_name) {
        update.display_name = Some(name);
    }
    if let Some(admin) = change.admin.filter(|_| !account.is_service()) {
        let role = if admin { Role::Admin } else { Role::User };
        if role != account.role {
            update.role = Some(role);
        }
    }
    let changed = update.disabled.is_some() || update.display_name.is_some() || update.role.is_some();
    if changed {
        let details = json!({
            "via": ACTOR,
            "disabled": update.disabled,
            "role": update.role.map(|role| if role == Role::Admin { "admin" } else { "user" }),
            "displayName": update.display_name.is_some(),
        });
        web.store().update_account(&account.login, update).await?;
        log(web, "account.update", &account.login, details).await;
    }
    if let Some(external) = change.external_id.filter(|external| *external != user.external_id) {
        web.store().scim_set_external_id(&user.id, external.clone()).await?;
        link_login(web, pairing, account, external.as_deref()).await?;
    }
    web.store().scim_touch(&user.id, flag).await?;
    self::user(web, &user.id).await
}

fn operations(body: &Value) -> ScimResult<&Vec<Value>> {
    body["Operations"]
        .as_array()
        .or_else(|| body["operations"].as_array())
        .ok_or_else(|| ScimError::bad("invalidSyntax", "A PATCH needs Operations."))
}

pub async fn patch_user(
    State(web): State<Web>,
    scim_auth: Scim,
    Path(id): Path<String>,
    body: Bytes,
) -> ScimResult<Response> {
    let body = body_json(&body)?;
    let user = user(&web, &id).await?;
    let mut change = PersonChange::default();
    for operation in operations(&body)? {
        let op = operation["op"].as_str().unwrap_or_default().to_ascii_lowercase();
        let remove = match op.as_str() {
            "add" | "replace" => false,
            "remove" => true,
            _ => return Err(ScimError::bad("invalidSyntax", "An operation is add, replace or remove.")),
        };
        match operation["path"].as_str().filter(|path| !path.is_empty()) {
            Some(path) => change_of(&user, &mut change, path, &operation["value"], remove)?,
            None => {
                let object = operation["value"]
                    .as_object()
                    .ok_or_else(|| ScimError::bad("invalidValue", "Without a path, the value is an object."))?;
                for (path, value) in object {
                    change_of(&user, &mut change, path, value, remove)?;
                }
            }
        }
    }
    let user = apply(&web, &scim_auth.pairing, user, change).await?;
    Ok(scim(StatusCode::OK, user_json(&user, &base(&web))))
}

pub async fn put_user(
    State(web): State<Web>,
    scim_auth: Scim,
    Path(id): Path<String>,
    body: Bytes,
) -> ScimResult<Response> {
    let body = body_json(&body)?;
    let user = user(&web, &id).await?;
    if let Some(name) = user_name(&body) {
        same_address(&user, &Value::String(name))?;
    }
    let change = PersonChange {
        // Without `active` the state stays as it is: a provider that leaves it out does not mean
        // to unlock anybody.
        active: boolean(&body["active"]),
        display_name: display_name(&body),
        external_id: Some(text(&body["externalId"], 255)),
        admin: names_admin(&body["roles"]),
    };
    let user = apply(&web, &scim_auth.pairing, user, change).await?;
    Ok(scim(StatusCode::OK, user_json(&user, &base(&web))))
}

pub async fn delete_user(State(web): State<Web>, _scim: Scim, Path(id): Path<String>) -> ScimResult<Response> {
    let user = user(&web, &id).await?;
    if !managed(&web, &user.account).await? {
        // Not the provider's to remove: it lets go of the account and the mailbox stays.
        web.store().scim_set_external_id(&user.id, None).await?;
        tracing::info!(login = %user.account.login, "SCIM: let go of an account it did not make");
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    web.store().trash_account(&user.account.login).await?;
    log(&web, "account.trash", &user.account.login, json!({ "via": ACTOR })).await;
    tracing::info!(login = %user.account.login, "SCIM: account moved to the trash");
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── Groups ────────────────────────────────────────────────

fn group_json(group: &ScimGroup, base: &str) -> Value {
    json!({
        "schemas": [GROUP_SCHEMA],
        "id": group.id,
        "displayName": group.display_name,
        "externalId": group.external_id,
        "members": group.members.iter().map(|id| json!({ "value": id })).collect::<Vec<_>>(),
        "meta": {
            "resourceType": "Group",
            "created": format_utc(group.created),
            "lastModified": format_utc(group.updated),
            "location": format!("{base}/Groups/{}", group.id),
        },
    })
}

/// The member ids of a `members` value.
fn member_ids(value: &Value) -> ScimResult<Vec<String>> {
    let list = match value {
        Value::Array(list) => list.as_slice(),
        Value::Object(_) => std::slice::from_ref(value),
        Value::Null => &[],
        _ => return Err(ScimError::bad("invalidValue", "members is a list of { value }.")),
    };
    if list.len() > SCIM_MAX_MEMBERS {
        return Err(ScimError::bad("tooMany", format!("A group has at most {SCIM_MAX_MEMBERS} members here.")));
    }
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|member| member["value"].as_str().or_else(|| member.as_str()))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && id.len() <= 255)
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn group_name(value: &Value) -> ScimResult<String> {
    text(value, 200).ok_or_else(|| ScimError::bad("invalidValue", "A group needs a displayName."))
}

fn group_conflict(error: StoreError) -> ScimError {
    match error {
        StoreError::Conflict(_) => ScimError::conflict("There is a group with this displayName already."),
        other => other.into(),
    }
}

pub async fn list_groups(State(web): State<Web>, _scim: Scim, Query(query): Query<ListQuery>) -> ScimResult<Response> {
    let filter = match query.filter()? {
        None => ScimGroupFilter::All,
        Some((attribute, value)) => match attribute.as_str() {
            "displayname" => ScimGroupFilter::DisplayName(value),
            "externalid" => ScimGroupFilter::ExternalId(value),
            "id" => ScimGroupFilter::Id(value),
            _ => return Err(ScimError::bad("invalidFilter", "Groups are filtered by displayName, externalId or id.")),
        },
    };
    let (start, count) = query.window();
    let (total, groups) = web.store().scim_groups(filter, start - 1, count).await?;
    let base = base(&web);
    Ok(list(groups.iter().map(|group| group_json(group, &base)).collect(), total, start))
}

pub async fn get_group(State(web): State<Web>, _scim: Scim, Path(id): Path<String>) -> ScimResult<Response> {
    let group = web.store().scim_group(&id).await?.ok_or_else(ScimError::not_found)?;
    Ok(scim(StatusCode::OK, group_json(&group, &base(&web))))
}

pub async fn create_group(State(web): State<Web>, _scim: Scim, body: Bytes) -> ScimResult<Response> {
    let body = body_json(&body)?;
    let group = web
        .store()
        .scim_create_group(
            &group_name(&body["displayName"])?,
            text(&body["externalId"], 255),
            member_ids(&body["members"])?,
        )
        .await
        .map_err(group_conflict)?;
    let base = base(&web);
    Ok(created(group_json(&group, &base), &format!("{base}/Groups/{}", group.id)))
}

async fn save_group(web: &Web, id: &str, update: ScimGroupUpdate) -> ScimResult<Response> {
    let group = web.store().scim_update_group(id, update).await.map_err(group_conflict)?;
    Ok(scim(StatusCode::OK, group_json(&group, &base(web))))
}

pub async fn put_group(
    State(web): State<Web>,
    _scim: Scim,
    Path(id): Path<String>,
    body: Bytes,
) -> ScimResult<Response> {
    let body = body_json(&body)?;
    web.store().scim_group(&id).await?.ok_or_else(ScimError::not_found)?;
    let update = ScimGroupUpdate {
        display_name: Some(group_name(&body["displayName"])?),
        external_id: Some(text(&body["externalId"], 255)),
        members: Some(member_ids(&body["members"])?),
    };
    save_group(&web, &id, update).await
}

/// `members[value eq "…"]`: the member a `remove` means.
fn member_in_path(path: &str) -> Option<String> {
    let inner = path.strip_prefix("members[")?.strip_suffix(']')?;
    parse_filter(inner).ok().filter(|(attribute, _)| attribute == "value").map(|(_, value)| value)
}

fn add_members(members: &mut Vec<String>, more: Vec<String>) {
    members.extend(more);
    members.sort();
    members.dedup();
}

pub async fn patch_group(
    State(web): State<Web>,
    _scim: Scim,
    Path(id): Path<String>,
    body: Bytes,
) -> ScimResult<Response> {
    let body = body_json(&body)?;
    let before = web.store().scim_group(&id).await?.ok_or_else(ScimError::not_found)?;
    let mut name = before.display_name.clone();
    let mut external = before.external_id.clone();
    let mut members = before.members.clone();
    for operation in operations(&body)? {
        let op = operation["op"].as_str().unwrap_or_default().to_ascii_lowercase();
        let value = &operation["value"];
        let path = operation["path"].as_str().unwrap_or_default().trim();
        let lower = path.to_ascii_lowercase();
        match (op.as_str(), lower.as_str()) {
            ("replace" | "add", "displayname") => name = group_name(value)?,
            ("replace" | "add", "externalid") => external = text(value, 255),
            ("remove", "externalid") => external = None,
            ("replace", "members") => members = member_ids(value)?,
            ("add", "members") => add_members(&mut members, member_ids(value)?),
            ("remove", "members") => {
                if value.is_null() {
                    members.clear();
                } else {
                    let gone = member_ids(value)?;
                    members.retain(|id| !gone.contains(id));
                }
            }
            ("remove", _) if lower.starts_with("members[") => {
                let gone = member_in_path(path)
                    .ok_or_else(|| ScimError::bad("invalidPath", "Only members[value eq \"…\"] is supported."))?;
                members.retain(|id| *id != gone);
            }
            ("replace" | "add", "") => {
                let object = value
                    .as_object()
                    .ok_or_else(|| ScimError::bad("invalidValue", "Without a path, the value is an object."))?;
                if let Some(value) = object.get("displayName") {
                    name = group_name(value)?;
                }
                if let Some(value) = object.get("externalId") {
                    external = text(value, 255);
                }
                if let Some(value) = object.get("members") {
                    let ids = member_ids(value)?;
                    if op == "add" {
                        add_members(&mut members, ids);
                    } else {
                        members = ids;
                    }
                }
            }
            ("add" | "replace" | "remove", _) => {}
            _ => return Err(ScimError::bad("invalidSyntax", "An operation is add, replace or remove.")),
        }
        if members.len() > SCIM_MAX_MEMBERS {
            return Err(ScimError::bad("tooMany", format!("A group has at most {SCIM_MAX_MEMBERS} members here.")));
        }
    }
    let update = ScimGroupUpdate {
        display_name: Some(name).filter(|name| *name != before.display_name),
        external_id: Some(external).filter(|external| *external != before.external_id),
        members: Some(members).filter(|members| *members != before.members),
    };
    save_group(&web, &id, update).await
}

pub async fn delete_group(State(web): State<Web>, _scim: Scim, Path(id): Path<String>) -> ScimResult<Response> {
    if !web.store().scim_delete_group(&id).await? {
        return Err(ScimError::not_found());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_rfc3339() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_793_279_045), "2026-10-29T13:04:05Z");
    }

    #[test]
    fn filters_are_attribute_eq_json_string() {
        assert_eq!(parse_filter("userName eq \"a@example.com\"").unwrap(), ("username".into(), "a@example.com".into()));
        assert_eq!(parse_filter("  externalId  EQ \"x \\\"y\\\"\" ").unwrap(), ("externalid".into(), "x \"y\"".into()));
        assert!(parse_filter("userName co \"a\"").is_err());
        assert!(parse_filter("userName eq a").is_err());
        assert!(parse_filter("userName").is_err());
    }

    #[test]
    fn roles_name_the_admin() {
        assert_eq!(names_admin(&json!([{ "value": "admin" }])), Some(true));
        assert_eq!(names_admin(&json!(["user", "Admin"])), Some(true));
        assert_eq!(names_admin(&json!([{ "value": "user" }])), Some(false));
        assert_eq!(names_admin(&json!([])), Some(false));
        assert_eq!(names_admin(&Value::Null), None);
    }

    #[test]
    fn members_are_ids_without_doubles() {
        assert_eq!(
            member_ids(&json!([{ "value": "b" }, { "value": "a" }, "b", { "value": " " }])).unwrap(),
            ["a", "b"]
        );
        assert_eq!(member_ids(&json!({ "value": "a" })).unwrap(), ["a"]);
        assert!(member_ids(&json!(5)).is_err());
        assert_eq!(member_in_path("members[value eq \"abc\"]"), Some("abc".into()));
        assert_eq!(member_in_path("members[display eq \"abc\"]"), None);
    }
}
