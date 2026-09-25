//! "Shared folders" in My account: sharing one's folders with people on this server, and what
//! others share with one (docs/sharing.md).

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{ImapMailbox, ShareLevel};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// Folder paths like `Projekte/UwUMail`, by mailbox id.
fn paths(mailboxes: &[ImapMailbox]) -> HashMap<i64, String> {
    let by_id: HashMap<i64, &ImapMailbox> = mailboxes.iter().map(|m| (m.id, m)).collect();
    mailboxes
        .iter()
        .map(|mailbox| {
            let mut names = vec![mailbox.name.clone()];
            let mut parent = mailbox.parent_id;
            while let Some(id) = parent {
                let Some(found) = by_id.get(&id) else { break };
                names.push(found.name.clone());
                parent = found.parent_id;
                if names.len() > 64 {
                    break;
                }
            }
            names.reverse();
            (mailbox.id, names.join("/"))
        })
        .collect()
}

async fn view(web: &Web, session: &Session) -> ApiResult<Value> {
    let store = web.store();
    let me = session.account.id;
    let mailboxes = store.imap_mailboxes(me).await?;
    let own_paths = paths(&mailboxes);
    let shares = store.shares_by_owner(me).await?;
    let folders: Vec<Value> = mailboxes
        .iter()
        .map(|mailbox| {
            let with: Vec<Value> = shares
                .iter()
                .filter(|share| share.mailbox_id == mailbox.id)
                .map(|share| {
                    json!({
                        "login": share.grantee_login,
                        "name": share.grantee_name,
                        "level": ShareLevel::of(&share.rights),
                        "rights": share.rights,
                    })
                })
                .collect();
            json!({
                "id": mailbox.id,
                "path": own_paths.get(&mailbox.id).cloned().unwrap_or_else(|| mailbox.name.clone()),
                "role": mailbox.role.map(|role| role.as_str()),
                "shares": with,
            })
        })
        .collect();

    let mut trees: HashMap<i64, HashMap<i64, String>> = HashMap::new();
    let mut shared_with_me = Vec::new();
    for entry in store.mailboxes_shared_with(me).await? {
        if let std::collections::hash_map::Entry::Vacant(vacant) = trees.entry(entry.owner_id) {
            vacant.insert(paths(&store.imap_mailboxes(entry.owner_id).await?));
        }
        let path = trees[&entry.owner_id].get(&entry.mailbox.id).cloned().unwrap_or_else(|| entry.mailbox.name.clone());
        shared_with_me.push(json!({
            "owner": entry.owner_login,
            "ownerName": entry.owner_name,
            "id": entry.mailbox.id,
            "path": path,
            "role": entry.mailbox.role.map(|role| role.as_str()),
            "level": ShareLevel::of(&entry.rights),
            "rights": entry.rights,
        }));
    }

    let people: Vec<Value> = store
        .share_people()
        .await?
        .into_iter()
        .filter(|person| person.id != me)
        .map(|person| json!({ "login": person.login, "name": person.display_name }))
        .collect();
    Ok(json!({ "folders": folders, "sharedWithMe": shared_with_me, "people": people }))
}

pub async fn show(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(view(&web, &session).await?))
}

#[derive(Deserialize)]
pub struct Share {
    login: String,
    level: String,
}

pub async fn share(
    State(web): State<Web>,
    session: Session,
    Path(mailbox): Path<i64>,
    Json(body): Json<Share>,
) -> ApiResult<Json<Value>> {
    let level = ShareLevel::parse(&body.level)
        .ok_or_else(|| ApiError::Invalid(format!("unknown level {:?}, expected read, write or all", body.level)))?;
    web.store().set_mailbox_acl(session.account.id, mailbox, body.login.trim(), level.rights()).await?;
    tracing::info!(login = %session.account.login, mailbox, with = %body.login.trim(), level = level.as_str(), "shared a folder");
    Ok(Json(view(&web, &session).await?))
}

pub async fn unshare(
    State(web): State<Web>,
    session: Session,
    Path((mailbox, login)): Path<(i64, String)>,
) -> ApiResult<Json<Value>> {
    web.store().set_mailbox_acl(session.account.id, mailbox, &login, "").await?;
    tracing::info!(login = %session.account.login, mailbox, with = %login, "stopped sharing a folder");
    Ok(Json(view(&web, &session).await?))
}
