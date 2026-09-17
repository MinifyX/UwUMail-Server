//! Word lists in My account and for admins: entries typed in or pasted, lists subscribed to by link, and the
//! built-in lists the server fetches itself.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::FEEDS;
use uwumail_store::{
    ListOwner, ListScope, WORD_LIST_ADMIN_LIMIT, WORD_LIST_PERSONAL_LIMIT, WORD_POINTS, WORD_POINTS_MAX,
    WORD_SOURCES_ADMIN_LIMIT, WORD_SOURCES_PERSONAL_LIMIT, WordSource,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::routes::audit;
use crate::session::{Admin, Session};

/// A list fetched this recently is not fetched again on request.
const REFRESH_PAUSE_SECS: i64 = 60;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWords {
    /// One entry per line.
    text: String,
    points: Option<f32>,
    #[serde(default)]
    note: String,
    /// Admins only: the domain the entries are for; the whole server when left out.
    domain: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSource {
    url: String,
    #[serde(default)]
    subject_only: bool,
    points: Option<f32>,
    /// Admins only, as for entries.
    domain: Option<String>,
}

async fn own_view(web: &Web, session: &Session) -> ApiResult<Value> {
    let scope = ListScope::Account(session.account.id);
    let store = web.store();
    Ok(json!({
        "entries": store.word_entries(scope).await?,
        "sources": store.word_sources(scope).await?,
        "limit": WORD_LIST_PERSONAL_LIMIT,
        "sourceLimit": WORD_SOURCES_PERSONAL_LIMIT,
        "defaultPoints": WORD_POINTS,
        "maxPoints": WORD_POINTS_MAX,
    }))
}

async fn admin_view(web: &Web) -> ApiResult<Value> {
    let store = web.store();
    let domains: Vec<String> = store.domains().await?.into_iter().map(|domain| domain.name).collect();
    Ok(json!({
        "entries": store.admin_word_entries().await?,
        "sources": store.admin_word_sources().await?,
        "domains": domains,
        "limit": WORD_LIST_ADMIN_LIMIT,
        "sourceLimit": WORD_SOURCES_ADMIN_LIMIT,
        "defaultPoints": WORD_POINTS,
        "maxPoints": WORD_POINTS_MAX,
    }))
}

async fn admin_scope(web: &Web, domain: Option<&str>) -> ApiResult<ListScope> {
    match domain {
        Some(name) => {
            let domain = web.store().domain(name).await?.ok_or_else(|| ApiError::NotFound(format!("domain {name}")))?;
            Ok(ListScope::Domain(domain.id))
        }
        None => Ok(ListScope::Server),
    }
}

/// Fetches a subscribed list, unless that just happened. Returns why it failed, if it did.
async fn refresh(web: &Web, source: &WordSource) -> Option<String> {
    if source.fetched_at.is_some_and(|at| crate::health::unix_now() - at < REFRESH_PAUSE_SECS) {
        return source.error.clone();
    }
    web.smtp().refresh_word_source(source).await.err()
}

async fn subscribe(
    web: &Web,
    scope: ListScope,
    new: NewSource,
    created_by: String,
) -> ApiResult<(WordSource, Option<String>)> {
    uwumail_smtp::Smtp::check_list_link(&new.url).map_err(|message| ApiError::Rule("wordSourceInvalid", message))?;
    let source = web.store().add_word_source(scope, new.url, new.subject_only, new.points, created_by).await?;
    let error = refresh(web, &source).await;
    Ok((source, error))
}

fn owned_source(source: Option<WordSource>, owner: ListOwner, id: i64) -> ApiResult<WordSource> {
    source
        .filter(|source| match (owner, source.scope) {
            (ListOwner::Admin, ListScope::Server | ListScope::Domain(_)) => true,
            (ListOwner::Account(owner), ListScope::Account(account)) => owner == account,
            _ => false,
        })
        .ok_or_else(|| ApiError::NotFound(format!("subscribed list {id}")))
}

pub async fn account_words(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(own_view(&web, &session).await?))
}

pub async fn account_add_words(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewWords>,
) -> ApiResult<Json<Value>> {
    let scope = ListScope::Account(session.account.id);
    let report = web.store().add_words(scope, new.text, new.points, new.note, session.account.login.clone()).await?;
    Ok(Json(json!({ "import": report, "lists": own_view(&web, &session).await? })))
}

pub async fn account_remove_word(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    web.store().remove_word_entry(ListOwner::Account(session.account.id), id).await?;
    Ok(Json(own_view(&web, &session).await?))
}

pub async fn account_subscribe(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewSource>,
) -> ApiResult<Json<Value>> {
    let scope = ListScope::Account(session.account.id);
    let (_, error) = subscribe(&web, scope, new, session.account.login.clone()).await?;
    Ok(Json(json!({ "error": error, "lists": own_view(&web, &session).await? })))
}

pub async fn account_refresh_source(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let source = owned_source(web.store().word_source(id).await?, ListOwner::Account(session.account.id), id)?;
    let error = refresh(&web, &source).await;
    Ok(Json(json!({ "error": error, "lists": own_view(&web, &session).await? })))
}

pub async fn account_unsubscribe(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    web.store().remove_word_source(ListOwner::Account(session.account.id), id).await?;
    Ok(Json(own_view(&web, &session).await?))
}

pub async fn admin_words(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(admin_view(&web).await?))
}

pub async fn admin_add_words(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewWords>,
) -> ApiResult<Json<Value>> {
    let scope = admin_scope(&web, new.domain.as_deref()).await?;
    let target = new.domain.clone().unwrap_or_else(|| "server".into());
    let report = web.store().add_words(scope, new.text, new.points, new.note, session.account.login.clone()).await?;
    if report.added > 0 {
        let details = json!({ "added": report.added, "domain": new.domain });
        audit(&web, &session, "spam.wordsAdd", &target, details).await;
    }
    Ok(Json(json!({ "import": report, "lists": admin_view(&web).await? })))
}

pub async fn admin_remove_word(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let entry = web.store().remove_word_entry(ListOwner::Admin, id).await?;
    audit(&web, &session, "spam.wordRemove", &entry.pattern, json!({ "domain": entry.domain })).await;
    Ok(Json(admin_view(&web).await?))
}

pub async fn admin_subscribe(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewSource>,
) -> ApiResult<Json<Value>> {
    let scope = admin_scope(&web, new.domain.as_deref()).await?;
    let domain = new.domain.clone();
    let (source, error) = subscribe(&web, scope, new, session.account.login.clone()).await?;
    audit(&web, &session, "spam.wordSourceAdd", &source.url, json!({ "domain": domain })).await;
    Ok(Json(json!({ "error": error, "lists": admin_view(&web).await? })))
}

pub async fn admin_refresh_source(
    State(web): State<Web>,
    _admin: Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let source = owned_source(web.store().word_source(id).await?, ListOwner::Admin, id)?;
    let error = refresh(&web, &source).await;
    Ok(Json(json!({ "error": error, "lists": admin_view(&web).await? })))
}

pub async fn admin_unsubscribe(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let source = web.store().remove_word_source(ListOwner::Admin, id).await?;
    audit(&web, &session, "spam.wordSourceRemove", &source.url, json!({ "domain": source.domain })).await;
    Ok(Json(admin_view(&web).await?))
}

async fn feeds_view(web: &Web) -> ApiResult<Value> {
    let config = web.smtp().spam_settings().feeds;
    let states = web.store().feed_states().await?;
    let feeds: Vec<Value> = FEEDS
        .iter()
        .map(|feed| {
            let state = states.iter().find(|state| state.key == feed.key);
            json!({
                "key": feed.key,
                "source": feed.source,
                "page": feed.page,
                "needsKey": feed.needs_key,
                "intervalSecs": feed.interval_secs,
                "active": feed.active(&config),
                "fetchedAt": state.and_then(|state| state.fetched_at),
                "changedAt": state.and_then(|state| state.changed_at),
                "error": state.and_then(|state| state.error.clone()),
                "entries": state.map_or(0, |state| state.entries),
            })
        })
        .collect();
    let key_set = config.abuse_ch_key.as_deref().is_some_and(|key| !key.trim().is_empty());
    Ok(json!({ "feeds": feeds, "abuseChKeySet": key_set }))
}

/// The built-in lists and how fetching them went.
pub async fn admin_feeds(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(feeds_view(&web).await?))
}

/// Fetches a built-in list now, if it is switched on.
pub async fn admin_refresh_feed(
    State(web): State<Web>,
    _admin: Admin,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    let feed = uwumail_smtp::feed(&key).ok_or_else(|| ApiError::NotFound(format!("built-in list {key}")))?;
    let config = web.smtp().spam_settings().feeds;
    if !feed.active(&config) {
        let message = format!("{} is switched off or waits for its Auth-Key", feed.source);
        return Err(ApiError::Rule("feedInactive", message));
    }
    let recent = web
        .store()
        .feed_states()
        .await?
        .into_iter()
        .find(|state| state.key == feed.key)
        .and_then(|state| state.fetched_at)
        .is_some_and(|at| crate::health::unix_now() - at < REFRESH_PAUSE_SECS);
    let error = if recent { None } else { web.smtp().refresh_feed(feed).await.err() };
    let mut view = feeds_view(&web).await?;
    view["error"] = json!(error);
    Ok(Json(view))
}
