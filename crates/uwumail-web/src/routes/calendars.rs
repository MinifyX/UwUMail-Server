//! "Calendars" in My account: sharing one's calendars and address books with people of this
//! server, and the ones others share with oneself (docs/calendars.md).
//!
//! Everything here answers for the logged-in account. The same shares are made over JMAP
//! (`shareWith`) by the webmail and the apps; this is the page for everyone else.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{DavKind, NewDavCollection, ShareRights};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

#[derive(Deserialize)]
pub struct ShareRequest {
    address: String,
    rights: ShareRights,
}

fn kinds(session: &Session) -> Vec<DavKind> {
    let protocols = &session.account.protocols;
    let mut kinds = Vec::new();
    if protocols.caldav {
        kinds.push(DavKind::Calendar);
    }
    if protocols.carddav {
        kinds.push(DavKind::Addressbook);
    }
    kinds
}

async fn view(web: &Web, session: &Session) -> ApiResult<Json<Value>> {
    let store = web.store();
    let account = session.account.id;
    let names = web.smtp().tone().language.collection_names();
    let shares = store.dav_shares(account, None).await?;
    let mut own = Vec::new();
    let mut shared = Vec::new();
    for kind in kinds(session) {
        let default = match kind {
            DavKind::Calendar => NewDavCollection::default_calendar(names.0),
            DavKind::Addressbook => NewDavCollection::default_address_book(names.1),
        };
        for collection in store.dav_collections(account, kind, default).await? {
            let with: Vec<Value> = shares
                .iter()
                .filter(|share| share.collection_id == collection.id)
                .map(|share| {
                    json!({
                        "accountId": share.account_id,
                        "address": share.login,
                        "name": share.display_name,
                        "rights": share.rights,
                    })
                })
                .collect();
            own.push(json!({
                "id": collection.id,
                "kind": kind,
                "name": if collection.display_name.is_empty() { &collection.slug } else { &collection.display_name },
                "color": collection.color,
                "entries": collection.resources,
                "shares": with,
            }));
        }
        for item in store.dav_shared_with(account, kind).await? {
            let collection = &item.collection;
            shared.push(json!({
                "id": collection.id,
                "kind": kind,
                "name": if collection.display_name.is_empty() { &collection.slug } else { &collection.display_name },
                "color": collection.color,
                "owner": item.owner_login,
                "ownerName": item.owner_name,
                "rights": item.rights,
            }));
        }
    }
    Ok(Json(json!({
        "calendars": session.account.protocols.caldav,
        "contacts": session.account.protocols.carddav,
        "own": own,
        "shared": shared,
    })))
}

/// One's own calendars and address books with who they are shared with, and those shared with
/// oneself.
pub async fn list(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    view(&web, &session).await
}

/// Shares one of one's collections with someone of the server, or changes what they may do.
pub async fn share(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(request): Json<ShareRequest>,
) -> ApiResult<Json<Value>> {
    let address = request.address.trim().to_lowercase();
    if address.is_empty() || !address.contains('@') {
        return Err(ApiError::Invalid("an address of someone on this server is needed".into()));
    }
    let owned = web.store().dav_access(session.account.id, id).await?;
    if !owned.is_some_and(|(_, access)| access.is_owner()) {
        return Err(ApiError::NotFound(format!("calendar {id}")));
    }
    web.store().dav_share(session.account.id, id, &address, request.rights).await?;
    view(&web, &session).await
}

/// Stops sharing one of one's collections with someone.
pub async fn unshare(
    State(web): State<Web>,
    session: Session,
    Path((id, account)): Path<(i64, i64)>,
) -> ApiResult<Json<Value>> {
    let owned = web.store().dav_access(session.account.id, id).await?;
    if !owned.is_some_and(|(_, access)| access.is_owner()) {
        return Err(ApiError::NotFound(format!("calendar {id}")));
    }
    web.store().dav_unshare(session.account.id, id, account).await?;
    view(&web, &session).await
}

/// Leaves a collection someone else shares with one.
pub async fn leave(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    web.store().dav_unshare(session.account.id, id, session.account.id).await?;
    view(&web, &session).await
}
