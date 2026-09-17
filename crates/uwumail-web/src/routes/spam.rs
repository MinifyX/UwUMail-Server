//! The spam filter in My account and for admins: what the Bayes filter learned, learning once from
//! mail that is already sorted into Junk or kept in the inbox, allowed and blocked senders, and one's
//! own spam limits.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{
    BAYES_FOLDER_LIMIT, BAYES_MIN_LEARNED, BAYES_WANTED_AFTER_SECS, ListOwner, ListScope, NewSenderListEntry,
    SENDER_LIST_ADMIN_LIMIT, SENDER_LIST_PERSONAL_LIMIT, SPAM_LIMIT_RANGE, SenderKind, SenderList, SpamLimits,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::routes::audit;
use crate::session::{Admin, Session};

/// Learning waits in a queue; past this many waiting messages, more requests only make it longer.
const BUSY_QUEUE: i64 = 20_000;

async fn not_busy(web: &Web) -> ApiResult<()> {
    if web.store().bayes_queue_length().await? > BUSY_QUEUE {
        return Err(ApiError::Rule("learningBusy", "the spam filter is still learning, try again later".into()));
    }
    Ok(())
}

/// One's own limits next to the server's, which apply where one set none.
async fn limits_json(web: &Web, account_id: i64) -> ApiResult<Value> {
    let spam = web.smtp().spam_settings();
    Ok(json!({
        "own": web.store().spam_limits(account_id).await?,
        "server": { "junk": spam.junk_score, "reject": spam.reject_score },
        "min": SPAM_LIMIT_RANGE.start(),
        "max": SPAM_LIMIT_RANGE.end(),
    }))
}

pub async fn account_overview(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let store = web.store();
    Ok(Json(json!({
        "limits": limits_json(&web, session.account.id).await?,
        "bayes": {
            "enabled": web.smtp().spam_settings().bayes,
            "minimum": BAYES_MIN_LEARNED,
            "own": store.bayes_totals(Some(session.account.id)).await?,
            "server": store.bayes_totals(None).await?,
        },
    })))
}

pub async fn account_set_limits(
    State(web): State<Web>,
    session: Session,
    Json(limits): Json<SpamLimits>,
) -> ApiResult<Json<Value>> {
    web.store().set_spam_limits(session.account.id, limits).await?;
    Ok(Json(limits_json(&web, session.account.id).await?))
}

/// Learns from one's own sorted mail, for the whole server and for oneself.
pub async fn account_learn(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    not_busy(&web).await?;
    let (spam, ham) =
        web.store().queue_bayes_from_folders(session.account.id, BAYES_WANTED_AFTER_SECS, BAYES_FOLDER_LIMIT).await?;
    Ok(Json(json!({ "spam": spam, "ham": ham })))
}

pub async fn admin_overview(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let store = web.store();
    Ok(Json(json!({
        "bayes": {
            "enabled": web.smtp().spam_settings().bayes,
            "minimum": BAYES_MIN_LEARNED,
            "server": store.bayes_totals(None).await?,
            "queued": store.bayes_queue_length().await?,
        },
    })))
}

/// Learns from everyone's sorted mail at once.
pub async fn admin_learn(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    not_busy(&web).await?;
    let store = web.store();
    let (mut spam, mut ham, mut people) = (0, 0, 0);
    for account in store.accounts().await? {
        let (found_spam, found_ham) =
            store.queue_bayes_from_folders(account.id, BAYES_WANTED_AFTER_SECS, BAYES_FOLDER_LIMIT).await?;
        spam += found_spam;
        ham += found_ham;
        if found_spam + found_ham > 0 {
            people += 1;
        }
    }
    let details = json!({ "spam": spam, "ham": ham, "people": people });
    audit(&web, &session, "spam.learnFromFolders", "server", details.clone()).await;
    Ok(Json(details))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSender {
    list: SenderList,
    /// Guessed from the value when left out.
    kind: Option<SenderKind>,
    value: String,
    #[serde(default)]
    note: String,
    /// Admins only: the domain the entry is for; the whole server when left out.
    domain: Option<String>,
}

async fn own_senders(web: &Web, session: &Session) -> ApiResult<Json<Value>> {
    let entries = web.store().sender_list(ListScope::Account(session.account.id)).await?;
    Ok(Json(json!({ "entries": entries, "limit": SENDER_LIST_PERSONAL_LIMIT })))
}

async fn admin_senders(web: &Web) -> ApiResult<Json<Value>> {
    let store = web.store();
    let entries = store.admin_sender_lists().await?;
    let domains: Vec<String> = store.domains().await?.into_iter().map(|domain| domain.name).collect();
    Ok(Json(json!({ "entries": entries, "domains": domains, "limit": SENDER_LIST_ADMIN_LIMIT })))
}

/// One's own allowed and blocked senders.
pub async fn account_senders(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    own_senders(&web, &session).await
}

pub async fn account_add_sender(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewSender>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    web.store()
        .add_sender_list_entry(NewSenderListEntry {
            scope: ListScope::Account(session.account.id),
            list: new.list,
            kind: new.kind,
            value: new.value,
            note: new.note,
            created_by: session.account.login.clone(),
        })
        .await?;
    Ok((StatusCode::CREATED, own_senders(&web, &session).await?))
}

pub async fn account_remove_sender(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    web.store().remove_sender_list_entry(ListOwner::Account(session.account.id), id).await?;
    own_senders(&web, &session).await
}

/// The allowed and blocked senders of the whole server and of every domain.
pub async fn admin_senders_view(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    admin_senders(&web).await
}

pub async fn admin_add_sender(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewSender>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let store = web.store();
    let scope = match new.domain.as_deref() {
        Some(name) => {
            let domain = store.domain(name).await?.ok_or_else(|| ApiError::NotFound(format!("domain {name}")))?;
            ListScope::Domain(domain.id)
        }
        None => ListScope::Server,
    };
    let entry = store
        .add_sender_list_entry(NewSenderListEntry {
            scope,
            list: new.list,
            kind: new.kind,
            value: new.value,
            note: new.note,
            created_by: session.account.login.clone(),
        })
        .await?;
    let details = json!({ "list": entry.list, "kind": entry.kind, "domain": entry.domain });
    audit(&web, &session, "spam.senderAdd", &entry.value, details).await;
    Ok((StatusCode::CREATED, admin_senders(&web).await?))
}

pub async fn admin_remove_sender(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let entry = web.store().remove_sender_list_entry(ListOwner::Admin, id).await?;
    let details = json!({ "list": entry.list, "kind": entry.kind, "domain": entry.domain });
    audit(&web, &session, "spam.senderRemove", &entry.value, details).await;
    admin_senders(&web).await
}
