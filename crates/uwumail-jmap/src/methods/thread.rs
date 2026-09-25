//! Thread/get (RFC 8621, section 3).

use serde_json::{Value, json};

use super::{Ctx, get_ids};
use crate::error::{MethodError, MethodResult};
use crate::ids;

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let requested = get_ids(args)?.ok_or_else(|| MethodError::new("requestTooLarge", "ask for specific thread ids"))?;
    let numbers: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('t', id)).collect();
    let mut threads = ctx.jmap.store.thread_emails(ctx.account.id, numbers).await?;
    if ctx.shared.is_some() {
        // In someone else's account a thread holds the emails the caller may read.
        for emails in threads.values_mut() {
            let records = ctx.jmap.store.emails_by_ids(ctx.account.id, emails.clone()).await?;
            let readable: Vec<i64> = super::email::visible_records(ctx, records).iter().map(|r| r.id).collect();
            emails.retain(|email| readable.contains(email));
        }
        threads.retain(|_, emails| !emails.is_empty());
    }
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in requested {
        match ctx.parse_id('t', &id).and_then(|n| threads.get(&n).map(|emails| (n, emails))) {
            Some((number, emails)) => {
                list.push(json!({ "id": ids::thread(number), "emailIds": emails.iter().map(|e| ids::email(*e)).collect::<Vec<_>>() }))
            }
            None => not_found.push(id),
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}
