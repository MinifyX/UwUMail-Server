//! A person's own mail in the server's backups (docs/jmap-backups.md): `BackupEmail/query` finds
//! it by its headers, `BackupEmail/get` shows it, `BackupEmail/restore` puts it back. Only ever the
//! mail of the logged-in account itself, never that of a shared one.

use mail_parser::MimeHeaders;
use serde_json::{Map, Value, json};
use uwumail_backup::{Backups, IndexedMail, Owner, Restored, Search};

use super::{Ctx, check_filter_size};
use crate::dates;
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// Each one is read from the backup storage, which may be far away.
const MAX_GET: usize = 10;
const MAX_RESTORE: usize = 50;
const MAX_TEXT: usize = 256;
/// The most of a mail's text a preview shows.
const MAX_PREVIEW_CHARS: usize = 100_000;

/// The backups, when people may search them, and whose mail this call may see.
async fn open(ctx: &Ctx<'_>, args: &Value) -> MethodResult<(Backups, Owner)> {
    if ctx.shared.is_some() {
        return Err(MethodError::new("forbidden", "only your own mail can be searched in the backups"));
    }
    ctx.check_account(args)?;
    let backups = ctx.jmap.backups.clone().ok_or_else(|| MethodError::kind("unknownMethod"))?;
    if !backups.user_search().await {
        return Err(MethodError::new("forbidden", "searching the backups is turned off"));
    }
    Ok((backups, Owner { account_id: ctx.account.id, created_at: ctx.account.created_at }))
}

fn failed(err: uwumail_backup::Error) -> MethodError {
    tracing::warn!(%err, "reading the backups for a person failed");
    MethodError::new("serverUnavailable", "the backups cannot be read right now")
}

fn backup_id(id: i64) -> String {
    format!("k{id}")
}

fn ids_arg(ctx: &Ctx<'_>, args: &Value, max: usize) -> MethodResult<Vec<(String, Option<i64>)>> {
    let list =
        args.get("ids").and_then(Value::as_array).ok_or_else(|| MethodError::invalid_arguments("ids is required"))?;
    if list.len() > max {
        return Err(MethodError::new("requestTooLarge", format!("at most {max} ids at once")));
    }
    list.iter()
        .map(|id| {
            let id = id.as_str().ok_or_else(|| MethodError::invalid_arguments("ids must be strings"))?;
            Ok((id.to_owned(), ctx.parse_id('k', id)))
        })
        .collect()
}

fn summary(mail: &IndexedMail) -> Value {
    json!({
        "id": backup_id(mail.id),
        "subject": mail.subject,
        "from": mail.from,
        "to": mail.to,
        "receivedAt": dates::format(mail.received_at),
        "size": mail.size,
        "folder": mail.folder,
        "snapshots": mail.snapshots.iter().map(|at| dates::format(*at)).collect::<Vec<_>>(),
    })
}

/// `BackupEmail/query`: filter `{text, after, before}`, newest first, with what of the backups is
/// indexed so far.
pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let (backups, owner) = open(ctx, args).await?;
    check_filter_size(args.get("filter"))?;
    let mut search = Search { text: None, after: None, before: None, limit: DEFAULT_LIMIT };
    if let Some(filter) = args.get("filter").filter(|f| !f.is_null()) {
        let filter = filter.as_object().ok_or_else(|| MethodError::kind("unsupportedFilter"))?;
        for (key, value) in filter {
            match (key.as_str(), value) {
                (_, Value::Null) => {}
                ("text", Value::String(text)) if text.chars().count() <= MAX_TEXT => {
                    search.text = Some(text.trim().to_owned()).filter(|t| !t.is_empty())
                }
                ("text", _) => return Err(MethodError::invalid_arguments("text must be a short string")),
                ("after" | "before", Value::String(date)) => {
                    let at = dates::parse(date).ok_or_else(|| MethodError::invalid_arguments("not a UTCDate"))?;
                    if key == "after" { search.after = Some(at) } else { search.before = Some(at) }
                }
                ("after" | "before", _) => return Err(MethodError::invalid_arguments("dates must be UTCDates")),
                (other, _) => return Err(MethodError::new("unsupportedFilter", format!("unknown filter {other}"))),
            }
        }
    }
    if let Some(limit) = args.get("limit").filter(|l| !l.is_null()) {
        let limit = limit.as_u64().ok_or_else(|| MethodError::invalid_arguments("limit must be a positive number"))?;
        search.limit = (limit as usize).min(MAX_LIMIT);
    }
    let found = backups.search_mail(owner, search).await.map_err(failed)?;
    let mut list = Vec::with_capacity(found.len());
    for mail in &found {
        let mut entry = summary(mail);
        entry["inMailbox"] = json!(in_mailbox(ctx, mail).await?);
        list.push(entry);
    }
    let (done, total) = backups.indexing();
    Ok(json!({
        "accountId": ctx.account_id(),
        "list": list,
        "indexing": { "done": done, "total": total },
    }))
}

async fn in_mailbox(ctx: &Ctx<'_>, mail: &IndexedMail) -> MethodResult<bool> {
    let Ok(hash) = uwumail_store::BlobHash::parse(&mail.blob_hash) else { return Ok(false) };
    Ok(ctx.jmap.store.holds_message(ctx.account.id, None, hash).await?)
}

/// `BackupEmail/get`: the mails with their text and the attachments they have.
pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let (backups, owner) = open(ctx, args).await?;
    let wanted = ids_arg(ctx, args, MAX_GET)?;
    let numbers: Vec<i64> = wanted.iter().filter_map(|(_, n)| *n).collect();
    let mut contents = backups.backup_mail_contents(owner, &numbers).await.map_err(failed)?.into_iter();
    let (mut list, mut not_found) = (Vec::new(), Vec::new());
    for (id, number) in wanted {
        let content = match number {
            Some(_) => contents.next().and_then(|(_, content)| content),
            None => None,
        };
        let Some((mail, raw)) = content else {
            not_found.push(id);
            continue;
        };
        let mut entry = summary(&mail);
        // Mail from before 0.16 may be nested deeper than is safe to parse: headers only, then.
        let parsed = uwumail_store::mime_limits::parse_message(&raw);
        let text = parsed.as_ref().and_then(|m| m.body_text(0)).unwrap_or_default();
        entry["textBody"] = json!(text.chars().take(MAX_PREVIEW_CHARS).collect::<String>());
        entry["attachments"] = json!(
            parsed
                .iter()
                .flat_map(|m| m.attachments())
                .map(|part| json!({
                    "name": part.attachment_name(),
                    "type": part.content_type().map(|t| match &t.c_subtype {
                        Some(sub) => format!("{}/{sub}", t.c_type),
                        None => t.c_type.to_string(),
                    }),
                    "size": part.len(),
                }))
                .collect::<Vec<_>>()
        );
        entry["inMailbox"] = json!(in_mailbox(ctx, &mail).await?);
        list.push(entry);
    }
    Ok(json!({ "accountId": ctx.account_id(), "list": list, "notFound": not_found }))
}

/// `BackupEmail/restore`: puts the mails back, into "Restored <date>".
pub async fn restore(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let (backups, owner) = open(ctx, args).await?;
    let wanted = ids_arg(ctx, args, MAX_RESTORE)?;
    let numbers: Vec<i64> = wanted.iter().filter_map(|(_, n)| *n).collect();
    let mut done = backups.restore_mails(owner, &numbers).await.map_err(failed)?.into_iter();
    let (mut restored, mut already, mut not_restored) = (Map::new(), Vec::new(), Map::new());
    for (id, number) in wanted {
        let outcome = match number {
            Some(_) => done.next().map_or(Restored::NotFound, |(_, outcome)| outcome),
            None => Restored::NotFound,
        };
        let error = match outcome {
            Restored::Into(folder) => {
                restored.insert(id, json!(ids::mailbox(folder)));
                continue;
            }
            Restored::AlreadyThere => {
                already.push(id);
                continue;
            }
            Restored::NotFound => SetError::not_found(),
            Restored::QuotaExceeded => SetError::new("overQuota", "the mailbox is full"),
            Restored::Unreadable => SetError::new("invalidEmail", "the mail cannot be read safely"),
        };
        not_restored.insert(id, error.to_json());
    }
    Ok(json!({
        "accountId": ctx.account_id(),
        "restored": restored,
        "alreadyThere": already,
        "notRestored": not_restored,
    }))
}
