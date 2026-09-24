//! The spam filter's rules — allowed and blocked senders and word list entries — as one table: searched,
//! filtered and paged on the server, changed one by one or many at once, imported from pasted lines and
//! exported as CSV. Admins see every scope (the whole server, each domain, each person); people their own.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{
    BulkAction, ListOwner, ListScope, NewSenderListEntry, RuleChange, RuleImport, RuleList, RuleQuery, RuleScope,
    RuleSort, RuleState, RuleType, ScopeFilter, SenderKind, SenderList, WORD_POINTS, WORD_POINTS_MAX,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::routes::audit;
use crate::session::{Admin, Session};

const PAGE_SIZES: [usize; 4] = [25, 50, 100, 250];

/// Who is asking: an admin for every scope, or a person for their own.
enum Who {
    Admin(Session),
    Person(Session),
}

impl Who {
    fn owner(&self) -> ListOwner {
        match self {
            Who::Admin(_) => ListOwner::Admin,
            Who::Person(session) => ListOwner::Account(session.account.id),
        }
    }

    fn session(&self) -> &Session {
        match self {
            Who::Admin(session) | Who::Person(session) => session,
        }
    }

    /// Admins' changes go into the change log; a person's own list is their own business.
    async fn audit(&self, web: &Web, action: &str, target: &str, details: Value) {
        if let Who::Admin(session) = self {
            audit(web, session, action, target, details).await;
        }
    }
}

/// `server`, `domain:<name>` or `account:<login>`.
async fn parse_scope(web: &Web, who: &Who, text: Option<&str>) -> ApiResult<ListScope> {
    if let Who::Person(session) = who {
        return Ok(ListScope::Account(session.account.id));
    }
    let text = text.unwrap_or("server").trim();
    if text.is_empty() || text == "server" {
        return Ok(ListScope::Server);
    }
    if let Some(name) = text.strip_prefix("domain:") {
        let domain = web.store().domain(name).await?.ok_or_else(|| ApiError::NotFound(format!("domain {name}")))?;
        return Ok(ListScope::Domain(domain.id));
    }
    if let Some(login) = text.strip_prefix("account:") {
        let account = web.store().account(login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
        return Ok(ListScope::Account(account.id));
    }
    Err(ApiError::Invalid(format!("unknown scope: {text}")))
}

async fn parse_filter(web: &Web, who: &Who, text: Option<&str>) -> ApiResult<ScopeFilter> {
    if matches!(who, Who::Person(_)) {
        return Ok(ScopeFilter::All);
    }
    Ok(match text.map(str::trim).unwrap_or("") {
        "" | "all" => ScopeFilter::All,
        "server" => ScopeFilter::Server,
        "domains" => ScopeFilter::Domains,
        "accounts" => ScopeFilter::Accounts,
        other => ScopeFilter::One(parse_scope(web, who, Some(other)).await?),
    })
}

fn split<T>(text: Option<&str>, parse: impl Fn(&str) -> Option<T>) -> ApiResult<Vec<T>> {
    text.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| parse(item).ok_or_else(|| ApiError::Invalid(format!("unknown filter: {item}"))))
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ListQuery {
    search: Option<String>,
    /// `all`, `server`, `domains`, `accounts`, `domain:<name>` or `account:<login>`.
    scope: Option<String>,
    /// Comma-separated: `allow`, `block`, `points`.
    list: Option<String>,
    /// Comma-separated sender kinds, `word` and `regex`.
    kind: Option<String>,
    state: Option<RuleState>,
    sort: Option<RuleSort>,
    desc: bool,
    page: usize,
    per_page: Option<usize>,
}

async fn rule_query(web: &Web, who: &Who, query: ListQuery) -> ApiResult<RuleQuery> {
    const KINDS: [&str; 7] = ["address", "domain", "pattern", "ip", "host", "word", "regex"];
    let lists = split(query.list.as_deref(), |item| match item {
        "allow" => Some(RuleList::Allow),
        "block" => Some(RuleList::Block),
        "points" => Some(RuleList::Points),
        _ => None,
    })?;
    let kinds = split(query.kind.as_deref(), |item| KINDS.contains(&item).then(|| item.to_owned()))?;
    let limit = query.per_page.filter(|size| PAGE_SIZES.contains(size)).unwrap_or(PAGE_SIZES[0]);
    Ok(RuleQuery {
        owner: Some(who.owner()),
        scope: parse_filter(web, who, query.scope.as_deref()).await?,
        search: query.search.unwrap_or_default(),
        lists,
        kinds,
        state: query.state,
        sort: query.sort.unwrap_or_default(),
        descending: query.desc,
        offset: query.page.saturating_mul(limit),
        limit,
    })
}

async fn list(web: &Web, who: &Who, query: ListQuery) -> ApiResult<Json<Value>> {
    let query = rule_query(web, who, query).await?;
    let (per_page, page) = (query.limit, query.offset / query.limit.max(1));
    let found = web.store().rules(query).await?;
    Ok(Json(json!({
        "rules": found.rules,
        "total": found.total,
        "lists": found.lists,
        "kinds": found.kinds,
        "page": page,
        "perPage": per_page,
        "pageSizes": PAGE_SIZES,
        "defaultPoints": WORD_POINTS,
        "maxPoints": WORD_POINTS_MAX,
    })))
}

pub async fn admin_list(
    State(web): State<Web>,
    Admin(session): Admin,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    list(&web, &Who::Admin(session), query).await
}

pub async fn account_list(
    State(web): State<Web>,
    session: Session,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    list(&web, &Who::Person(session), query).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopesQuery {
    #[serde(default)]
    search: String,
}

/// The whole server, the domains and the people, with how many rules each has, for the scope picker.
pub async fn admin_scopes(
    State(web): State<Web>,
    _admin: Admin,
    Query(query): Query<ScopesQuery>,
) -> ApiResult<Json<Value>> {
    let scopes = web.store().rule_scopes(query.search, 50).await?;
    let scopes: Vec<Value> = scopes
        .into_iter()
        .map(|(scope, count)| json!({ "key": scope.key(), "scope": scope, "count": count }))
        .collect();
    Ok(Json(json!({ "scopes": scopes })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRule {
    #[serde(rename = "type")]
    rule_type: RuleType,
    /// Senders: `allow` or `block`.
    #[serde(default = "block")]
    list: SenderList,
    kind: Option<SenderKind>,
    value: String,
    #[serde(default)]
    note: String,
    points: Option<f32>,
    scope: Option<String>,
    expires_at: Option<i64>,
}

fn block() -> SenderList {
    SenderList::Block
}

fn check_expiry(expires_at: Option<i64>) -> ApiResult<()> {
    match expires_at {
        Some(at) if at <= crate::health::unix_now() => {
            Err(ApiError::Rule("ruleInvalid", "the end date has to be in the future".into()))
        }
        _ => Ok(()),
    }
}

async fn create(web: &Web, who: &Who, new: NewRule) -> ApiResult<(StatusCode, Json<Value>)> {
    check_expiry(new.expires_at)?;
    let scope = parse_scope(web, who, new.scope.as_deref()).await?;
    let created_by = who.session().account.login.clone();
    let store = web.store();
    let rule = match new.rule_type {
        RuleType::Sender => {
            let entry = store
                .add_sender_list_entry(NewSenderListEntry {
                    scope,
                    list: new.list,
                    kind: new.kind,
                    value: new.value,
                    note: new.note,
                    created_by,
                    expires_at: new.expires_at,
                })
                .await?;
            store.rule(RuleType::Sender, entry.id).await?
        }
        RuleType::Word => {
            if new.value.lines().filter(|line| !line.trim().is_empty()).count() != 1 {
                return Err(ApiError::Rule(
                    "ruleInvalid",
                    "one word or expression at a time; paste more with Import".into(),
                ));
            }
            let report = store
                .add_words_until(scope, new.value.clone(), new.points, new.note, created_by, new.expires_at)
                .await?;
            if let Some(refused) = report.refused.first() {
                return Err(ApiError::Rule("wordInvalid", refused.reason.clone()));
            }
            if report.duplicates > 0 {
                return Err(ApiError::Rule("wordListed", format!("{} is already on the list", new.value.trim())));
            }
            let query = RuleQuery {
                owner: Some(who.owner()),
                scope: ScopeFilter::One(scope),
                lists: vec![RuleList::Points],
                sort: RuleSort::Created,
                descending: true,
                limit: 1,
                ..RuleQuery::default()
            };
            store.rules(query).await?.rules.pop()
        }
    };
    let rule = rule.ok_or(ApiError::Internal)?;
    let details = json!({ "type": rule.rule_type, "list": rule.list, "kind": rule.kind, "scope": rule.scope.key() });
    who.audit(web, "spam.ruleAdd", &rule.value, details).await;
    Ok((StatusCode::CREATED, Json(json!(rule))))
}

pub async fn admin_create(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(new): Json<NewRule>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    create(&web, &Who::Admin(session), new).await
}

pub async fn account_create(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewRule>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    create(&web, &Who::Person(session), new).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    #[serde(flatten)]
    change: RuleChange,
    /// Admins only: moves the rule.
    scope: Option<String>,
}

async fn change(web: &Web, who: &Who, rule_type: RuleType, id: i64, request: Change) -> ApiResult<Json<Value>> {
    let mut change = request.change;
    if let Some(Some(at)) = change.expires_at {
        check_expiry(Some(at))?;
    }
    if request.scope.is_some() {
        change.scope = Some(parse_scope(web, who, request.scope.as_deref()).await?);
    }
    let rule = web.store().change_rule(who.owner(), rule_type, id, change).await?;
    let details = json!({ "type": rule.rule_type, "list": rule.list, "scope": rule.scope.key() });
    who.audit(web, "spam.ruleChange", &rule.value, details).await;
    Ok(Json(json!(rule)))
}

pub async fn admin_change(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((rule_type, id)): Path<(RuleType, i64)>,
    Json(request): Json<Change>,
) -> ApiResult<Json<Value>> {
    change(&web, &Who::Admin(session), rule_type, id, request).await
}

pub async fn account_change(
    State(web): State<Web>,
    session: Session,
    Path((rule_type, id)): Path<(RuleType, i64)>,
    Json(request): Json<Change>,
) -> ApiResult<Json<Value>> {
    if request.scope.is_some() {
        return Err(ApiError::Invalid("your own rules stay yours".into()));
    }
    change(&web, &Who::Person(session), rule_type, id, request).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    #[serde(rename = "type")]
    rule_type: RuleType,
    id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bulk {
    items: Vec<Item>,
    /// `delete`, `allow`, `block`, `scope` or `expiry`.
    action: String,
    scope: Option<String>,
    expires_at: Option<i64>,
}

async fn bulk(web: &Web, who: &Who, request: Bulk) -> ApiResult<Json<Value>> {
    let action = match request.action.as_str() {
        "delete" => BulkAction::Delete,
        "allow" => BulkAction::SetList(SenderList::Allow),
        "block" => BulkAction::SetList(SenderList::Block),
        "scope" if matches!(who, Who::Admin(_)) => {
            BulkAction::SetScope(parse_scope(web, who, request.scope.as_deref()).await?)
        }
        "expiry" => {
            check_expiry(request.expires_at)?;
            BulkAction::SetExpiry(request.expires_at)
        }
        other => return Err(ApiError::Invalid(format!("unknown action: {other}"))),
    };
    let items: Vec<(RuleType, i64)> = request.items.iter().map(|item| (item.rule_type, item.id)).collect();
    let count = items.len();
    let report = web.store().bulk_rules(who.owner(), items, action).await?;
    let details =
        json!({ "rules": count, "changed": report.changed, "scope": request.scope, "expiresAt": request.expires_at });
    who.audit(web, "spam.ruleBulk", &request.action, details).await;
    Ok(Json(json!(report)))
}

pub async fn admin_bulk(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<Bulk>,
) -> ApiResult<Json<Value>> {
    bulk(&web, &Who::Admin(session), request).await
}

pub async fn account_bulk(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<Bulk>,
) -> ApiResult<Json<Value>> {
    bulk(&web, &Who::Person(session), request).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    #[serde(rename = "type")]
    rule_type: RuleType,
    #[serde(default = "block")]
    list: SenderList,
    text: String,
    #[serde(default)]
    note: String,
    points: Option<f32>,
    scope: Option<String>,
    expires_at: Option<i64>,
}

async fn import(web: &Web, who: &Who, request: Import) -> ApiResult<Json<Value>> {
    check_expiry(request.expires_at)?;
    let scope = parse_scope(web, who, request.scope.as_deref()).await?;
    let report = web
        .store()
        .import_rules(RuleImport {
            rule_type: request.rule_type,
            scope,
            list: request.list,
            text: request.text,
            note: request.note,
            points: request.points,
            expires_at: request.expires_at,
            created_by: who.session().account.login.clone(),
        })
        .await?;
    let details = json!({ "type": request.rule_type, "added": report.added, "scope": request.scope });
    who.audit(web, "spam.ruleImport", "", details).await;
    Ok(Json(json!(report)))
}

pub async fn admin_import(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<Import>,
) -> ApiResult<Json<Value>> {
    import(&web, &Who::Admin(session), request).await
}

pub async fn account_import(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<Import>,
) -> ApiResult<Json<Value>> {
    import(&web, &Who::Person(session), request).await
}

/// A CSV field, quoted when it has to be, and never read as a formula by a spreadsheet.
fn csv(field: &str) -> String {
    let field = if field.starts_with(['=', '+', '-', '@']) { format!("'{field}") } else { field.to_owned() };
    if field.contains([',', '"', '\n', '\r']) { format!("\"{}\"", field.replace('"', "\"\"")) } else { field }
}

/// Unix seconds as `2026-09-24T15:04:05Z`, empty for none.
fn date(at: Option<i64>) -> String {
    let Some(at) = at else { return String::new() };
    let (days, seconds) = (at.div_euclid(86_400), at.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

async fn export(web: &Web, who: &Who, query: ListQuery) -> ApiResult<Response> {
    let query = rule_query(web, who, query).await?;
    let rules = web.store().all_rules(query).await?;
    let mut text =
        String::from("type,list,kind,value,scope,note,points,expires_at,hits,last_hit_at,created_at,created_by\n");
    for rule in &rules {
        let scope = match &rule.scope {
            RuleScope::Server => "server".to_owned(),
            other => other.key(),
        };
        let fields = [
            if rule.rule_type == RuleType::Word { "word".to_owned() } else { "sender".to_owned() },
            serde_json::to_value(rule.list).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
            rule.kind.clone(),
            rule.value.clone(),
            scope,
            rule.note.clone(),
            rule.points.map(|points| points.to_string()).unwrap_or_default(),
            date(rule.expires_at),
            rule.hits.to_string(),
            date(rule.last_hit_at),
            date(Some(rule.created_at)),
            rule.created_by.clone(),
        ];
        text.push_str(&fields.iter().map(|field| csv(field)).collect::<Vec<_>>().join(","));
        text.push('\n');
    }
    who.audit(web, "spam.ruleExport", "", json!({ "rules": rules.len() })).await;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"uwumail-spam-rules.csv\""),
        ],
        text,
    )
        .into_response())
}

pub async fn admin_export(
    State(web): State<Web>,
    Admin(session): Admin,
    Query(query): Query<ListQuery>,
) -> ApiResult<Response> {
    export(&web, &Who::Admin(session), query).await
}

pub async fn account_export(
    State(web): State<Web>,
    session: Session,
    Query(query): Query<ListQuery>,
) -> ApiResult<Response> {
    export(&web, &Who::Person(session), query).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_are_plain_csv() {
        assert_eq!(date(Some(0)), "1970-01-01T00:00:00Z");
        assert_eq!(date(Some(1_790_262_245)), "2026-09-24T15:04:05Z");
        assert_eq!(date(None), "");
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("=cmd()"), "'=cmd()", "no formulas in a spreadsheet");
        assert_eq!(csv("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
