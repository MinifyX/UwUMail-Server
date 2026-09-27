//! Groups of a domain and shared mailboxes, for admins (docs/groups.md).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{GroupUpdate, NewGroup, NewSharedMailbox, WhoMaySend};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;

fn who_may_send(value: &str) -> ApiResult<WhoMaySend> {
    WhoMaySend::parse(value).ok_or_else(|| ApiError::Invalid("who may send is anyone, members or domain".into()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewGroupBody {
    /// The part before the @; the domain is the one in the path.
    local: String,
    #[serde(default)]
    name: String,
    #[serde(default = "anyone")]
    who_may_send: String,
    #[serde(default)]
    members_may_send_as: bool,
    #[serde(default)]
    members: Vec<String>,
}

fn anyone() -> String {
    "anyone".into()
}

pub async fn create_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<NewGroupBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let domain = super::domains::load(&web, &name).await?;
    let local = body.local.trim();
    if local.is_empty() || local.contains('@') {
        return Err(ApiError::Invalid("give the part before the @".into()));
    }
    let group = web
        .store()
        .create_group(NewGroup {
            address: format!("{local}@{}", domain.name),
            name: body.name,
            who_may_send: who_may_send(&body.who_may_send)?,
            members_may_send_as: body.members_may_send_as,
            members: body.members,
        })
        .await?;
    let members: Vec<&str> = group.members.iter().map(|member| member.login.as_str()).collect();
    let details = json!({
        "name": group.name,
        "whoMaySend": group.who_may_send,
        "membersMaySendAs": group.members_may_send_as,
        "members": members,
    });
    audit(&web, &session, "group.create", &group.address, details).await;
    Ok((StatusCode::CREATED, Json(json!(group))))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupChanges {
    name: Option<String>,
    who_may_send: Option<String>,
    members_may_send_as: Option<bool>,
    members: Option<Vec<String>>,
}

pub async fn update_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
    Json(changes): Json<GroupChanges>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let update = GroupUpdate {
        name: changes.name.clone(),
        who_may_send: changes.who_may_send.as_deref().map(who_may_send).transpose()?,
        members_may_send_as: changes.members_may_send_as,
        members: changes.members.clone(),
    };
    let group = web.store().update_group(&format!("{local}@{}", domain.name), update).await?;
    let mut details = serde_json::Map::new();
    if let Some(name) = changes.name {
        details.insert("name".into(), json!(name.trim()));
    }
    if changes.who_may_send.is_some() {
        details.insert("whoMaySend".into(), json!(group.who_may_send));
    }
    if let Some(send_as) = changes.members_may_send_as {
        details.insert("membersMaySendAs".into(), json!(send_as));
    }
    if changes.members.is_some() {
        let members: Vec<&str> = group.members.iter().map(|member| member.login.as_str()).collect();
        details.insert("members".into(), json!(members));
    }
    audit(&web, &session, "group.update", &group.address, Value::Object(details)).await;
    Ok(Json(json!(group)))
}

pub async fn remove_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let domain = super::domains::load(&web, &name).await?;
    let address = format!("{local}@{}", domain.name);
    web.store().delete_group(&address).await?;
    audit(&web, &session, "group.remove", &address, json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Switch {
    on: bool,
}

/// Opens a domain for masked addresses, or closes it for new ones.
pub async fn set_masked_addresses(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(body): Json<Switch>,
) -> ApiResult<StatusCode> {
    let domain = super::domains::load(&web, &name).await?;
    web.store().set_domain_masked_addresses(&domain.name, body.on).await?;
    audit(&web, &session, "domain.maskedAddresses", &domain.name, json!({ "on": body.on })).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    login: String,
    #[serde(default)]
    may_send: bool,
}

fn members(list: Vec<Member>) -> Vec<(String, bool)> {
    list.into_iter().map(|member| (member.login, member.may_send)).collect()
}

pub async fn shared_mailboxes(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!(web.store().shared_mailboxes().await?)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSharedMailboxBody {
    address: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    quota_bytes: i64,
    #[serde(default)]
    members: Vec<Member>,
}

pub async fn create_shared_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewSharedMailboxBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let account = web
        .store()
        .create_shared_mailbox(NewSharedMailbox {
            address: new.address.trim().to_owned(),
            name: new.name,
            quota_bytes: new.quota_bytes,
            members: members(new.members),
        })
        .await?;
    let members = web.store().shared_mailbox_members(account.id).await?;
    let details = json!({
        "quotaBytes": account.quota_bytes,
        "members": members.iter().map(|m| json!({ "login": m.login, "maySend": m.may_send })).collect::<Vec<_>>(),
    });
    audit(&web, &session, "sharedMailbox.create", &account.login, details).await;
    let person = web.store().person(&account.login).await?.ok_or(ApiError::Internal)?;
    Ok((StatusCode::CREATED, Json(json!({ "person": super::people::person_json(&person), "members": members }))))
}

#[derive(Deserialize)]
pub struct MembersBody {
    members: Vec<Member>,
}

pub async fn set_shared_mailbox_members(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(body): Json<MembersBody>,
) -> ApiResult<Json<Value>> {
    let members = web.store().set_shared_mailbox_members(&login, members(body.members)).await?;
    let details = json!({
        "members": members.iter().map(|m| json!({ "login": m.login, "maySend": m.may_send })).collect::<Vec<_>>(),
    });
    audit(&web, &session, "sharedMailbox.members", &login.to_lowercase(), details).await;
    Ok(Json(json!(members)))
}

/// Turns a person or a service into a shared mailbox with these members.
pub async fn make_shared_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(body): Json<MembersBody>,
) -> ApiResult<Json<Value>> {
    if login.eq_ignore_ascii_case(&session.account.login) {
        return Err(ApiError::Rule("notYourself", "you cannot turn your own account into a shared mailbox".into()));
    }
    let before = web.store().person(&login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
    let account = web.store().make_shared_mailbox(&login, members(body.members)).await?;
    let members = web.store().shared_mailbox_members(account.id).await?;
    let details = json!({
        "from": if before.account.is_service() { "service" } else { "person" },
        "members": members.iter().map(|m| json!({ "login": m.login, "maySend": m.may_send })).collect::<Vec<_>>(),
    });
    audit(&web, &session, "sharedMailbox.convert", &account.login, details).await;
    let person = web.store().person(&account.login).await?.ok_or(ApiError::Internal)?;
    Ok(Json(json!({ "person": super::people::person_json(&person), "members": members })))
}

/// Turns a shared mailbox back into a plain service.
pub async fn end_shared_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = web.store().end_shared_mailbox(&login).await?;
    audit(&web, &session, "sharedMailbox.end", &account.login, json!({})).await;
    let person = web.store().person(&account.login).await?.ok_or(ApiError::Internal)?;
    Ok(Json(super::people::person_json(&person)))
}
