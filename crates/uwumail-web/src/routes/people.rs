//! People: list, invite, change, lock out, trash and restore, addresses.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{AccountUpdate, NewAccount, PasswordLinkPurpose, Person, Role, TRASH_RETENTION_SECS};

use super::{audit, check_password};
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

/// Invitation and reset links work this long.
pub const PASSWORD_LINK_LIFETIME_SECS: i64 = 7 * 24 * 3600;

pub fn person_json(person: &Person) -> Value {
    let account = &person.account;
    let status = if account.deleted_at.is_some() {
        "deleted"
    } else if account.disabled {
        "disabled"
    } else if !person.has_password {
        "invited"
    } else {
        "active"
    };
    json!({
        "login": account.login,
        "name": account.display_name,
        "role": account.role,
        "status": status,
        "quotaBytes": account.quota_bytes,
        "usedBytes": account.used_bytes,
        "createdAt": account.created_at,
        "deletedAt": account.deleted_at,
        "purgeAt": account.deleted_at.map(|at| at + TRASH_RETENTION_SECS),
        "addresses": person.addresses,
    })
}

async fn load(web: &Web, login: &str) -> ApiResult<Person> {
    web.store().person(login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))
}

fn link_json(token: &str, expires_at: i64) -> Value {
    json!({ "path": format!("/password/{token}"), "expiresAt": expires_at })
}

pub async fn list(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let people = web.store().people().await?;
    Ok(Json(Value::Array(people.iter().map(person_json).collect())))
}

pub async fn detail(State(web): State<Web>, _admin: Admin, Path(login): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(person_json(&load(&web, &login).await?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPerson {
    address: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    quota_bytes: i64,
    /// Without a password the person gets an invitation link.
    password: Option<String>,
}

pub async fn create(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewPerson>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if let Some(password) = &new.password {
        check_password(password, &new.address)?;
    }
    let invite = new.password.is_none();
    let account = web
        .store()
        .create_account(NewAccount {
            address: new.address.trim().to_owned(),
            display_name: new.name,
            password: new.password,
            role: if new.admin { Role::Admin } else { Role::User },
            quota_bytes: new.quota_bytes,
        })
        .await?;
    let link = if invite {
        let (token, expires_at) = web
            .store()
            .create_password_link(
                &account.login,
                PasswordLinkPurpose::Invite,
                Some(session.account.id),
                PASSWORD_LINK_LIFETIME_SECS,
            )
            .await?;
        Some(link_json(&token, expires_at))
    } else {
        None
    };
    audit(
        &web,
        &session,
        "account.create",
        &account.login,
        json!({ "role": account.role, "quotaBytes": account.quota_bytes, "invited": invite }),
    )
    .await;
    let person = load(&web, &account.login).await?;
    Ok((StatusCode::CREATED, Json(json!({ "person": person_json(&person), "link": link }))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonChanges {
    name: Option<String>,
    admin: Option<bool>,
    quota_bytes: Option<i64>,
    disabled: Option<bool>,
}

pub async fn update(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(changes): Json<PersonChanges>,
) -> ApiResult<Json<Value>> {
    if changes.disabled == Some(true) && login.eq_ignore_ascii_case(&session.account.login) {
        return Err(ApiError::Rule("notYourself", "you cannot lock yourself out".into()));
    }
    let account = web
        .store()
        .update_account(
            &login,
            AccountUpdate {
                display_name: changes.name.clone(),
                role: changes.admin.map(|admin| if admin { Role::Admin } else { Role::User }),
                quota_bytes: changes.quota_bytes,
                disabled: changes.disabled,
            },
        )
        .await?;
    let mut details = serde_json::Map::new();
    if let Some(name) = changes.name {
        details.insert("name".into(), name.into());
    }
    if let Some(admin) = changes.admin {
        details.insert("admin".into(), admin.into());
    }
    if let Some(quota) = changes.quota_bytes {
        details.insert("quotaBytes".into(), quota.into());
    }
    if let Some(disabled) = changes.disabled {
        details.insert("disabled".into(), disabled.into());
    }
    audit(&web, &session, "account.update", &account.login, Value::Object(details)).await;
    Ok(Json(person_json(&load(&web, &account.login).await?)))
}

pub async fn trash(State(web): State<Web>, Admin(session): Admin, Path(login): Path<String>) -> ApiResult<Json<Value>> {
    if login.eq_ignore_ascii_case(&session.account.login) {
        return Err(ApiError::Rule("notYourself", "you cannot delete your own account".into()));
    }
    let account = web.store().trash_account(&login).await?;
    audit(&web, &session, "account.trash", &account.login, json!({})).await;
    Ok(Json(person_json(&load(&web, &account.login).await?)))
}

pub async fn restore(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = web.store().restore_account(&login).await?;
    audit(&web, &session, "account.restore", &account.login, json!({})).await;
    Ok(Json(person_json(&load(&web, &account.login).await?)))
}

#[derive(Deserialize)]
pub struct Confirmation {
    confirm: String,
}

/// Deletes a person and all their mail right away. The address must be typed in as confirmation.
pub async fn purge(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(confirmation): Json<Confirmation>,
) -> ApiResult<StatusCode> {
    let person = load(&web, &login).await?;
    if !confirmation.confirm.trim().eq_ignore_ascii_case(&person.account.login) {
        return Err(ApiError::Rule("confirmationMismatch", "type the address to confirm".into()));
    }
    if person.account.login == session.account.login {
        return Err(ApiError::Rule("notYourself", "you cannot delete your own account".into()));
    }
    // Going through the trash first applies its rules, such as keeping the last admin.
    web.store().trash_account(&person.account.login).await?;
    web.store().delete_account(&person.account.login).await?;
    audit(&web, &session, "account.purge", &person.account.login, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn password_link(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let person = load(&web, &login).await?;
    let purpose = if person.has_password { PasswordLinkPurpose::Reset } else { PasswordLinkPurpose::Invite };
    let (token, expires_at) = web
        .store()
        .create_password_link(&person.account.login, purpose, Some(session.account.id), PASSWORD_LINK_LIFETIME_SECS)
        .await?;
    audit(&web, &session, "account.passwordLink", &person.account.login, json!({ "purpose": purpose })).await;
    Ok(Json(link_json(&token, expires_at)))
}

#[derive(Deserialize)]
pub struct NewPassword {
    password: String,
}

/// Pro mode: an admin sets a password directly. The person is logged out everywhere.
pub async fn set_password(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(new): Json<NewPassword>,
) -> ApiResult<StatusCode> {
    let person = load(&web, &login).await?;
    check_password(&new.password, &person.account.login)?;
    web.store().set_password(&person.account.login, &new.password).await?;
    web.store().delete_web_sessions(person.account.id).await?;
    audit(&web, &session, "account.passwordSet", &person.account.login, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct NewAlias {
    address: String,
}

pub async fn add_alias(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(alias): Json<NewAlias>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let person = load(&web, &login).await?;
    if person.account.deleted_at.is_some() {
        return Err(ApiError::Invalid(format!("{login} is in the trash")));
    }
    web.store().add_alias(alias.address.trim(), &person.account.login).await?;
    audit(&web, &session, "alias.add", alias.address.trim(), json!({ "account": person.account.login })).await;
    Ok((StatusCode::CREATED, Json(person_json(&load(&web, &login).await?))))
}

pub async fn remove_alias(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((login, address)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let person = load(&web, &login).await?;
    let normalized = uwumail_store::normalize_address(&address)?;
    let address = format!("{}@{}", normalized.0, normalized.1);
    if !person.addresses.iter().any(|a| a.address == address && a.kind == "alias") {
        return Err(ApiError::NotFound(format!("alias {address} of {login}")));
    }
    web.store().remove_alias(&address).await?;
    audit(&web, &session, "alias.remove", &address, json!({ "account": person.account.login })).await;
    Ok(Json(person_json(&load(&web, &login).await?)))
}
