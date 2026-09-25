//! /queryChanges (RFC 8620, section 5.6) for Email, Mailbox, EmailSubmission, SieveScript and
//! ContactCard.
//!
//! The query state is the account's state, and the change log says which objects changed since.
//! Every changed object is reported as `removed`, and those of them that are in the results now
//! are `added` again at their new place — the RFC allows reporting more removals than strictly
//! happened. That is exact as long as an object's place in the results depends only on the object
//! itself. Where it also depends on others, their objects count as changed too: all mail of a
//! thread in which something changed when threads are collapsed or matched by keyword, all
//! mailboxes when they are sorted or filtered as a tree. Other queries (CalendarEvent, whose
//! expanded recurrences are not objects of their own) say `canCalculateChanges: false` and get
//! `cannotCalculateChanges` here.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use uwumail_store::{EmailFilter, EmailSortProperty, StoreError};

use super::{Ctx, address_book, contact_card, email, mailbox, sieve, submission};
use crate::error::{MethodError, MethodResult};
use crate::ids;

/// Whether `<type>/queryChanges` can answer for this /query method. The /query
/// responses say so in `canCalculateChanges`.
pub fn can_calculate(method: &str) -> bool {
    matches!(
        method,
        "Email/query" | "Mailbox/query" | "EmailSubmission/query" | "SieveScript/query" | "ContactCard/query"
    )
}

/// The type a queryChanges method is for, with its change log kind and id prefix.
fn kind_of(method: &str) -> Option<(&'static str, char)> {
    Some(match method {
        "Email/queryChanges" => ("Email", 'e'),
        "Mailbox/queryChanges" => ("Mailbox", 'm'),
        "EmailSubmission/queryChanges" => ("EmailSubmission", 's'),
        "SieveScript/queryChanges" => ("SieveScript", 'r'),
        "ContactCard/queryChanges" => ("ContactCard", 'k'),
        _ => return None,
    })
}

/// The arguments of the /query call that gives the whole result: no window into it.
fn whole_query(args: &Value) -> Value {
    let mut query = args.as_object().cloned().unwrap_or_default();
    for key in ["position", "anchor", "anchorOffset", "limit", "sinceQueryState", "maxChanges", "upToId"] {
        query.remove(key);
    }
    query.insert("calculateTotal".into(), json!(true));
    Value::Object(query)
}

/// Whether an email filter looks at other mail of the thread.
fn filter_uses_threads(filter: &EmailFilter) -> bool {
    match filter {
        EmailFilter::And(list) | EmailFilter::Or(list) | EmailFilter::Not(list) => list.iter().any(filter_uses_threads),
        EmailFilter::AllInThreadHaveKeyword(_)
        | EmailFilter::SomeInThreadHaveKeyword(_)
        | EmailFilter::NoneInThreadHaveKeyword(_) => true,
        _ => false,
    }
}

/// The whole current result of the query, in order, and the ids whose place may have moved
/// besides the changed ones.
async fn current_results(
    ctx: &mut Ctx<'_>,
    method: &str,
    args: &Value,
    changed: &BTreeSet<String>,
    since: i64,
) -> MethodResult<(Vec<String>, BTreeSet<String>)> {
    let mut extra = BTreeSet::new();
    let query = whole_query(args);
    let ids = match method {
        "Email/queryChanges" => {
            let filter =
                query.get("filter").filter(|f| !f.is_null()).map(|f| email::parse_filter(ctx, f)).transpose()?;
            let sort = email::parse_sort(query.get("sort"))?;
            let collapse = query.get("collapseThreads").and_then(Value::as_bool).unwrap_or(false);
            let by_thread = collapse
                || filter.as_ref().is_some_and(filter_uses_threads)
                || sort.iter().any(|s| {
                    matches!(
                        s.property,
                        EmailSortProperty::AllInThreadHaveKeyword(_) | EmailSortProperty::SomeInThreadHaveKeyword(_)
                    )
                });
            let results = ctx.jmap.store.query_emails(ctx.account.id, filter, sort, collapse).await?;
            if by_thread {
                // Threads in which something changed: their other mail may have moved too.
                let mut threads: BTreeSet<i64> = ctx
                    .jmap
                    .store
                    .changes(ctx.account.id, "Thread", since, 0)
                    .await
                    .map_err(changes_error)?
                    .into_iter_all()
                    .collect();
                let numbers: Vec<i64> = changed.iter().filter_map(|id| ids::parse('e', id)).collect();
                for record in ctx.jmap.store.emails_by_ids(ctx.account.id, numbers).await? {
                    threads.insert(record.thread_id);
                }
                let members = ctx.jmap.store.thread_emails(ctx.account.id, threads.into_iter().collect()).await?;
                extra.extend(members.values().flatten().map(|id| ids::email(*id)));
            }
            results.into_iter().map(|(id, _)| ids::email(id)).collect()
        }
        _ => {
            let response = match method {
                "Mailbox/queryChanges" => mailbox::query(ctx, &query).await?,
                "EmailSubmission/queryChanges" => submission::query(ctx, &query).await?,
                "SieveScript/queryChanges" => sieve::query(ctx, &query).await?,
                "ContactCard/queryChanges" => {
                    address_book::check_enabled(ctx)?;
                    contact_card::query(ctx, &query).await?
                }
                _ => return Err(MethodError::kind("unknownMethod")),
            };
            let ids: Vec<String> = response["ids"]
                .as_array()
                .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            // A result the query cut short cannot be compared.
            if response.get("total").and_then(Value::as_u64).is_some_and(|total| total as usize != ids.len()) {
                return Err(MethodError::kind("cannotCalculateChanges"));
            }
            let as_tree = ["sortAsTree", "filterAsTree"].iter().any(|key| query.get(*key) == Some(&json!(true)));
            if method == "Mailbox/queryChanges" && as_tree {
                // A mailbox's place in a tree depends on its parents.
                extra.extend(ids.iter().cloned());
            }
            ids
        }
    };
    Ok((ids, extra))
}

fn changes_error(err: StoreError) -> MethodError {
    match err {
        StoreError::Invalid(_) => MethodError::kind("cannotCalculateChanges"),
        other => other.into(),
    }
}

trait AllChanged {
    fn into_iter_all(self) -> std::vec::IntoIter<i64>;
}

impl AllChanged for uwumail_store::Changes {
    fn into_iter_all(self) -> std::vec::IntoIter<i64> {
        let mut all = self.created;
        all.extend(self.updated);
        all.extend(self.destroyed);
        all.into_iter()
    }
}

pub async fn query_changes(ctx: &mut Ctx<'_>, method: &str, args: &Value) -> MethodResult<Value> {
    let (kind, prefix) = kind_of(method).ok_or_else(|| MethodError::kind("unknownMethod"))?;
    let query_method = method.replace("/queryChanges", "/query");
    if !can_calculate(&query_method) {
        return Err(MethodError::kind("cannotCalculateChanges"));
    }
    let since_text = args
        .get("sinceQueryState")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("sinceQueryState is required"))?;
    let since: i64 = since_text.parse().map_err(|_| MethodError::kind("cannotCalculateChanges"))?;
    let max_changes = match args.get("maxChanges") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(0) | None => return Err(MethodError::invalid_arguments("maxChanges must be a positive integer")),
            Some(max) => Some(max as usize),
        },
    };
    super::check_filter_size(args.get("filter"))?;
    // Read before the results, like /query: what changes in between is reported again next time.
    let new_state = ctx.state().await?;
    let changes = ctx.jmap.store.changes(ctx.account.id, kind, since, 0).await.map_err(changes_error)?;
    let changed: BTreeSet<String> = changes.into_iter_all().map(|id| format!("{prefix}{id}")).collect();
    let (results, extra) = current_results(ctx, method, args, &changed, since).await?;

    let removed: BTreeSet<String> = changed.into_iter().chain(extra).collect();
    let added: Vec<Value> = results
        .iter()
        .enumerate()
        .filter(|(_, id)| removed.contains(*id))
        .map(|(index, id)| json!({ "id": id, "index": index }))
        .collect();
    // Only what was in the results before matters to the client, but the old results are gone;
    // ids it never had are ignored by it.
    let removed: Vec<String> = removed.into_iter().collect();
    if max_changes.is_some_and(|max| removed.len() + added.len() > max) {
        return Err(MethodError::kind("tooManyChanges"));
    }
    let mut response = Map::new();
    response.insert("accountId".into(), json!(ctx.account_id()));
    response.insert("oldQueryState".into(), json!(since_text));
    response.insert("newQueryState".into(), json!(new_state));
    if args.get("calculateTotal").and_then(Value::as_bool).unwrap_or(false) {
        response.insert("total".into(), json!(results.len()));
    }
    response.insert("removed".into(), json!(removed));
    response.insert("added".into(), json!(added));
    Ok(Value::Object(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_dropped_for_the_whole_result() {
        let query =
            whole_query(&json!({ "accountId": "a1", "position": 5, "limit": 10, "anchor": "e1", "filter": {} }));
        assert_eq!(query, json!({ "accountId": "a1", "filter": {}, "calculateTotal": true }));
    }

    #[test]
    fn calendar_queries_cannot_be_calculated() {
        assert!(can_calculate("Email/query"));
        assert!(!can_calculate("CalendarEvent/query"));
    }

    #[test]
    fn thread_keyword_filters_are_found_inside_operators() {
        let nested =
            EmailFilter::Not(vec![EmailFilter::And(vec![EmailFilter::SomeInThreadHaveKeyword("$flagged".into())])]);
        assert!(filter_uses_threads(&nested));
        assert!(!filter_uses_threads(&EmailFilter::HasKeyword("$seen".into())));
    }
}
