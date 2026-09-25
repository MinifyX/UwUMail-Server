//! Email/get, Email/query, Email/set, Email/import and Email/parse (RFC 8621, section 4).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use uwumail_store::{
    EmailFilter, EmailRecord, EmailSort, EmailSortProperty, EmailUpdate, IngestRequest, KeywordsChange, MailboxTarget,
    MailboxesChange, StoreError,
};

use crate::sharing::SharedView;

use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, properties};
use crate::email::{self as email_json, BlobSource, BodyValueOptions, DEFAULT_BODY_PROPERTIES, DEFAULT_PROPERTIES};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{dates, ids};

const MAX_QUERY_LIMIT: usize = 5000;

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let requested = get_ids(args)?.ok_or_else(|| MethodError::new("requestTooLarge", "ask for specific email ids"))?;
    let properties = properties(args, "properties", DEFAULT_PROPERTIES)?;
    let body_properties = super::properties(args, "bodyProperties", DEFAULT_BODY_PROPERTIES)?;
    let options = BodyValueOptions {
        text: args.get("fetchTextBodyValues").and_then(Value::as_bool).unwrap_or(false),
        html: args.get("fetchHTMLBodyValues").and_then(Value::as_bool).unwrap_or(false),
        all: args.get("fetchAllBodyValues").and_then(Value::as_bool).unwrap_or(false),
        max_bytes: args.get("maxBodyValueBytes").and_then(Value::as_u64).unwrap_or(0) as usize,
    };
    let numbers: Vec<i64> = requested.iter().filter_map(|id| ctx.parse_id('e', id)).collect();
    let records = visible_records(ctx, ctx.jmap.store.emails_by_ids(ctx.account.id, numbers).await?);
    let needs_raw = email_json::needs_raw(&properties);

    let mut list = Vec::with_capacity(records.len());
    let mut not_found = Vec::new();
    for id in requested {
        let Some(record) = ctx.parse_id('e', &id).and_then(|n| records.iter().find(|r| r.id == n)) else {
            not_found.push(id);
            continue;
        };
        let raw = if needs_raw { Some(ctx.jmap.store.blob(&record.blob).await?) } else { None };
        list.push(email_json::to_json(
            Some(record),
            raw.as_deref(),
            &record.blob,
            &properties,
            &body_properties,
            options,
        ));
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

/// In someone else's account: only the emails the caller may read, each in only the mailboxes
/// shared with them.
pub fn visible_records(ctx: &Ctx<'_>, mut records: Vec<EmailRecord>) -> Vec<EmailRecord> {
    if let Some(view) = &ctx.shared {
        records.retain(|record| view.may_read_email(&record.mailbox_ids));
        for record in &mut records {
            record.mailbox_ids.retain(|mailbox| view.visible(*mailbox));
        }
    }
    records
}

/// Checks an update to an email in someone else's account against the rights, and keeps it from
/// touching the mailboxes the caller does not see.
fn check_shared_update(view: &SharedView, record: &EmailRecord, update: &mut EmailUpdate) -> Result<(), SetError> {
    if !view.may_read_email(&record.mailbox_ids) {
        return Err(SetError::not_found());
    }
    let forbidden = |what: &str| SetError::new("forbidden", format!("you may not {what} in this shared mailbox"));
    let changed: Vec<String> = match &update.keywords {
        KeywordsChange::Keep => Vec::new(),
        KeywordsChange::Patch(list) => {
            list.iter().filter(|(k, on)| record.keywords.contains(k) != *on).map(|(k, _)| k.clone()).collect()
        }
        KeywordsChange::Replace(list) => {
            let now: HashSet<&String> = list.iter().collect();
            let before: HashSet<&String> = record.keywords.iter().collect();
            now.symmetric_difference(&before).map(|k| (*k).clone()).collect()
        }
    };
    if changed.iter().any(|keyword| !view.may_change_keyword(&record.mailbox_ids, keyword)) {
        return Err(forbidden("change these keywords"));
    }
    let (added, removed): (Vec<i64>, Vec<i64>) = match &update.mailboxes {
        MailboxesChange::Keep => (Vec::new(), Vec::new()),
        MailboxesChange::Patch(list) => (
            list.iter().filter(|(m, on)| *on && !record.mailbox_ids.contains(m)).map(|(m, _)| *m).collect(),
            list.iter().filter(|(m, on)| !*on && record.mailbox_ids.contains(m)).map(|(m, _)| *m).collect(),
        ),
        MailboxesChange::Replace(list) => (
            list.iter().filter(|m| !record.mailbox_ids.contains(m)).copied().collect(),
            record.mailbox_ids.iter().filter(|m| view.visible(**m) && !list.contains(m)).copied().collect(),
        ),
    };
    if added.iter().any(|mailbox| !view.may(*mailbox, "i")) {
        return Err(forbidden("add messages"));
    }
    if removed.iter().any(|mailbox| !view.may(*mailbox, "te")) {
        return Err(forbidden("remove messages"));
    }
    if matches!(update.mailboxes, MailboxesChange::Replace(_)) {
        // Mailboxes of the owner that are not shared keep the email.
        let patch = added.into_iter().map(|m| (m, true)).chain(removed.into_iter().map(|m| (m, false))).collect();
        update.mailboxes = MailboxesChange::Patch(patch);
    }
    Ok(())
}

/// For a new email in someone else's account: every mailbox takes messages (`i`) and every
/// keyword may be set there.
pub(super) fn check_shared_create(
    ctx: &Ctx<'_>,
    mailboxes: &[MailboxTarget],
    keywords: &[String],
) -> Result<(), SetError> {
    let Some(view) = &ctx.shared else {
        return Ok(());
    };
    let ids: Vec<i64> =
        mailboxes.iter().map(|target| if let MailboxTarget::Id(id) = target { *id } else { -1 }).collect();
    if ids.iter().any(|mailbox| !view.may(*mailbox, "i")) {
        return Err(SetError::new("forbidden", "you may not add messages to this shared mailbox"));
    }
    if keywords.iter().any(|keyword| !view.may_change_keyword(&ids, keyword)) {
        return Err(SetError::new("forbidden", "you may not set these keywords in this shared mailbox"));
    }
    Ok(())
}

fn keyword(value: &Value, name: &str) -> MethodResult<String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{name} must be a string")))
}

pub(super) fn parse_filter(ctx: &Ctx<'_>, value: &Value) -> MethodResult<EmailFilter> {
    let object = value.as_object().ok_or_else(|| MethodError::new("unsupportedFilter", "filters must be objects"))?;
    if let Some(operator) = object.get("operator").and_then(Value::as_str) {
        let conditions = object
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::new("unsupportedFilter", "operators need conditions"))?
            .iter()
            .map(|c| parse_filter(ctx, c))
            .collect::<MethodResult<Vec<_>>>()?;
        return match operator {
            "AND" => Ok(EmailFilter::And(conditions)),
            "OR" => Ok(EmailFilter::Or(conditions)),
            "NOT" => Ok(EmailFilter::Not(conditions)),
            other => Err(MethodError::new("unsupportedFilter", format!("unknown operator {other}"))),
        };
    }
    let mut conditions = Vec::new();
    for (key, value) in object {
        let text = || {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a string")))
        };
        let date = || {
            value
                .as_str()
                .and_then(dates::parse)
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a UTC date")))
        };
        let number =
            || value.as_i64().ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a number")));
        let condition = match key.as_str() {
            // A mailbox that does not exist matches nothing.
            "inMailbox" => EmailFilter::InMailbox(value.as_str().and_then(|v| ctx.parse_id('m', v)).unwrap_or(-1)),
            "inMailboxOtherThan" => EmailFilter::InMailboxOtherThan(
                value
                    .as_array()
                    .map(|list| list.iter().filter_map(|v| v.as_str().and_then(|v| ctx.parse_id('m', v))).collect())
                    .unwrap_or_default(),
            ),
            "before" => EmailFilter::Before(date()?),
            "after" => EmailFilter::After(date()?),
            "minSize" => EmailFilter::MinSize(number()?),
            "maxSize" => EmailFilter::MaxSize(number()?),
            "hasKeyword" => EmailFilter::HasKeyword(keyword(value, key)?),
            "notKeyword" => EmailFilter::NotKeyword(keyword(value, key)?),
            "allInThreadHaveKeyword" => EmailFilter::AllInThreadHaveKeyword(keyword(value, key)?),
            "someInThreadHaveKeyword" => EmailFilter::SomeInThreadHaveKeyword(keyword(value, key)?),
            "noneInThreadHaveKeyword" => EmailFilter::NoneInThreadHaveKeyword(keyword(value, key)?),
            "hasAttachment" => EmailFilter::HasAttachment(value.as_bool().unwrap_or(false)),
            "text" => EmailFilter::Text(text()?),
            "from" => EmailFilter::From(text()?),
            "to" => EmailFilter::To(text()?),
            "cc" => EmailFilter::Cc(text()?),
            "bcc" => EmailFilter::Bcc(text()?),
            "subject" => EmailFilter::Subject(text()?),
            "body" => EmailFilter::Body(text()?),
            other => return Err(MethodError::new("unsupportedFilter", format!("the filter {other} is not supported"))),
        };
        conditions.push(condition);
    }
    Ok(if conditions.len() == 1 { conditions.remove(0) } else { EmailFilter::And(conditions) })
}

pub(super) fn parse_sort(value: Option<&Value>) -> MethodResult<Vec<EmailSort>> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    list.iter()
        .map(|comparator| {
            let ascending = comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true);
            let keyword = || {
                comparator
                    .get("keyword")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| MethodError::kind("unsupportedSort"))
            };
            let property = match comparator.get("property").and_then(Value::as_str) {
                Some("receivedAt") => EmailSortProperty::ReceivedAt,
                Some("sentAt") => EmailSortProperty::SentAt,
                Some("size") => EmailSortProperty::Size,
                Some("from") => EmailSortProperty::From,
                Some("to") => EmailSortProperty::To,
                Some("subject") => EmailSortProperty::Subject,
                Some("hasKeyword") => EmailSortProperty::HasKeyword(keyword()?),
                Some("allInThreadHaveKeyword") => EmailSortProperty::AllInThreadHaveKeyword(keyword()?),
                Some("someInThreadHaveKeyword") => EmailSortProperty::SomeInThreadHaveKeyword(keyword()?),
                other => return Err(MethodError::new("unsupportedSort", format!("cannot sort by {other:?}"))),
            };
            Ok(EmailSort { property, ascending })
        })
        .collect()
}

/// A query's filter, in someone else's account limited to what is in a mailbox the caller may
/// read.
pub(super) fn scoped_filter(ctx: &Ctx<'_>, filter: Option<EmailFilter>) -> Option<EmailFilter> {
    let Some(view) = &ctx.shared else {
        return filter;
    };
    let mut readable: Vec<EmailFilter> = view.readable().into_iter().map(EmailFilter::InMailbox).collect();
    if readable.is_empty() {
        readable.push(EmailFilter::InMailbox(-1));
    }
    let scope = EmailFilter::Or(readable);
    Some(match filter {
        Some(filter) => EmailFilter::And(vec![scope, filter]),
        None => scope,
    })
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let filter = args.get("filter").filter(|f| !f.is_null()).map(|f| parse_filter(ctx, f)).transpose()?;
    let filter = scoped_filter(ctx, filter);
    let sort = parse_sort(args.get("sort"))?;
    let collapse = args.get("collapseThreads").and_then(Value::as_bool).unwrap_or(false);
    let results = ctx.jmap.store.query_emails(ctx.account.id, filter, sort, collapse).await?;
    let total = results.len();

    let limit =
        args.get("limit").and_then(Value::as_u64).map_or(MAX_QUERY_LIMIT, |l| (l as usize).min(MAX_QUERY_LIMIT));
    let mut position = match args.get("anchor").and_then(Value::as_str) {
        Some(anchor) => {
            let anchor_id = ctx.parse_id('e', anchor).ok_or_else(|| MethodError::kind("anchorNotFound"))?;
            let index = results
                .iter()
                .position(|(id, _)| *id == anchor_id)
                .ok_or_else(|| MethodError::kind("anchorNotFound"))?;
            let offset = args.get("anchorOffset").and_then(Value::as_i64).unwrap_or(0);
            (index as i64 + offset).max(0) as usize
        }
        None => {
            let requested = args.get("position").and_then(Value::as_i64).unwrap_or(0);
            if requested < 0 { total.saturating_sub(requested.unsigned_abs() as usize) } else { requested as usize }
        }
    };
    position = position.min(total);
    let ids: Vec<String> = results.iter().skip(position).take(limit).map(|(id, _)| ids::email(*id)).collect();

    let mut response = json!({
        "accountId": ctx.account_id(),
        "queryState": state,
        "canCalculateChanges": false,
        "position": position,
        "ids": ids,
    });
    if args.get("calculateTotal").and_then(Value::as_bool).unwrap_or(false) {
        response["total"] = json!(total);
    }
    if args.get("limit").and_then(Value::as_u64).is_some_and(|l| l as usize > MAX_QUERY_LIMIT) {
        response["limit"] = json!(MAX_QUERY_LIMIT);
    }
    Ok(response)
}

/// Blob contents for building messages, loaded before the (synchronous) builder runs.
struct LoadedBlobs(HashMap<String, Vec<u8>>);

impl BlobSource for LoadedBlobs {
    fn blob(&self, blob_id: &str) -> Option<Vec<u8>> {
        self.0.get(blob_id).cloned()
    }
}

fn collect_blob_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(blob) = map.get("blobId").and_then(Value::as_str) {
                out.push(blob.to_owned());
            }
            map.values().for_each(|v| collect_blob_ids(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_blob_ids(v, out)),
        _ => {}
    }
}

/// Reads a blob the account may access: a whole blob or one part of a stored message.
pub async fn read_blob(ctx: &Ctx<'_>, blob_id: &str) -> Option<Vec<u8>> {
    let reference = ids::parse_blob(ctx.resolve(blob_id)?)?;
    let accessible = match &ctx.shared {
        None => ctx.jmap.store.blob_accessible(ctx.account.id, reference.hash()).await.ok()?,
        // In someone else's account: the caller's own uploads and mail, and what is in a mailbox
        // they may read.
        Some(view) => {
            ctx.jmap.store.blob_accessible(view.me.id, reference.hash()).await.ok()?
                || ctx.jmap.store.blob_in_mailboxes(reference.hash(), view.readable()).await.ok()?
        }
    };
    if !accessible {
        return None;
    }
    let bytes = ctx.jmap.store.blob(reference.hash()).await.ok()?;
    match reference {
        ids::BlobRef::Whole(_) => Some(bytes),
        ids::BlobRef::Part(_, index) => email_json::part_content(&bytes, index).map(|(content, _)| content),
    }
}

pub(super) fn mailbox_ids(ctx: &Ctx<'_>, value: Option<&Value>) -> Result<Vec<MailboxTarget>, SetError> {
    let map = value
        .and_then(Value::as_object)
        .ok_or_else(|| SetError::invalid_properties(&["mailboxIds"], "mailboxIds is required"))?;
    let mut targets = Vec::new();
    for (id, present) in map {
        if present != &Value::Bool(true) {
            continue;
        }
        let number = ctx
            .parse_id('m', id)
            .ok_or_else(|| SetError::invalid_properties(&["mailboxIds"], format!("mailbox {id} does not exist")))?;
        targets.push(MailboxTarget::Id(number));
    }
    if targets.is_empty() {
        return Err(SetError::invalid_properties(&["mailboxIds"], "an email needs at least one mailbox"));
    }
    Ok(targets)
}

pub(super) fn keywords(value: Option<&Value>) -> Result<Vec<String>, SetError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Object(map)) => {
            Ok(map.iter().filter(|(_, v)| **v == Value::Bool(true)).map(|(k, _)| k.clone()).collect())
        }
        Some(_) => Err(SetError::invalid_properties(&["keywords"], "keywords must be an object")),
    }
}

pub(super) fn received_at(value: Option<&Value>) -> Result<Option<i64>, SetError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(date)) => dates::parse(date)
            .map(Some)
            .ok_or_else(|| SetError::invalid_properties(&["receivedAt"], "receivedAt must be a UTC date")),
        Some(_) => Err(SetError::invalid_properties(&["receivedAt"], "receivedAt must be a date")),
    }
}

pub(super) fn created_json(email: &uwumail_store::IngestedEmail) -> Value {
    json!({ "id": ids::email(email.id), "blobId": ids::blob(&email.blob), "threadId": ids::thread(email.thread_id), "size": email.size })
}

fn patch_to_update(ctx: &Ctx<'_>, id: i64, patch: &Map<String, Value>) -> Result<EmailUpdate, SetError> {
    let mut update = EmailUpdate { id, ..EmailUpdate::default() };
    let mut keyword_patch = Vec::new();
    let mut mailbox_patch = Vec::new();
    for (key, value) in patch {
        if key == "keywords" {
            let list = keywords(Some(value))?;
            update.keywords = KeywordsChange::Replace(list);
        } else if let Some(keyword) = key.strip_prefix("keywords/") {
            match value {
                Value::Bool(true) => keyword_patch.push((keyword.to_owned(), true)),
                Value::Null | Value::Bool(false) => keyword_patch.push((keyword.to_owned(), false)),
                _ => return Err(SetError::new("invalidPatch", format!("{key} must be true or null"))),
            }
        } else if key == "mailboxIds" {
            let targets = mailbox_ids(ctx, Some(value))?;
            update.mailboxes = MailboxesChange::Replace(
                targets.into_iter().filter_map(|t| if let MailboxTarget::Id(n) = t { Some(n) } else { None }).collect(),
            );
        } else if let Some(mailbox) = key.strip_prefix("mailboxIds/") {
            let number = ctx.parse_id('m', mailbox).ok_or_else(|| {
                SetError::invalid_properties(&["mailboxIds"], format!("mailbox {mailbox} does not exist"))
            })?;
            match value {
                Value::Bool(true) => mailbox_patch.push((number, true)),
                Value::Null | Value::Bool(false) => mailbox_patch.push((number, false)),
                _ => return Err(SetError::new("invalidPatch", format!("{key} must be true or null"))),
            }
        } else {
            return Err(SetError::invalid_properties(&[key.as_str()], format!("{key} cannot be changed")));
        }
    }
    if !keyword_patch.is_empty() {
        if update.keywords != KeywordsChange::Keep {
            return Err(SetError::new("invalidPatch", "keywords and keywords/... cannot be combined"));
        }
        update.keywords = KeywordsChange::Patch(keyword_patch);
    }
    if !mailbox_patch.is_empty() {
        if update.mailboxes != MailboxesChange::Keep {
            return Err(SetError::new("invalidPatch", "mailboxIds and mailboxIds/... cannot be combined"));
        }
        update.mailboxes = MailboxesChange::Patch(mailbox_patch);
    }
    Ok(update)
}

/// Applies an Email/set update map; used by Email/set and EmailSubmission's onSuccessUpdateEmail.
pub async fn apply_updates(ctx: &Ctx<'_>, update: &Map<String, Value>, response: &mut SetResponse) -> MethodResult<()> {
    let mut updates = Vec::new();
    let mut update_ids = Vec::new();
    for (id, patch) in update {
        let parsed = match ctx.parse_id('e', id) {
            Some(number) => patch
                .as_object()
                .ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))
                .and_then(|patch| patch_to_update(ctx, number, patch)),
            None => Err(SetError::not_found()),
        };
        match parsed {
            Ok(update) => {
                updates.push(update);
                update_ids.push(id.clone());
            }
            Err(err) => {
                response.not_updated.insert(id.clone(), err.to_json());
            }
        }
    }
    if let Some(view) = &ctx.shared {
        let records = ctx.jmap.store.emails_by_ids(ctx.account.id, updates.iter().map(|u| u.id).collect()).await?;
        let mut allowed = Vec::new();
        let mut allowed_ids = Vec::new();
        for (mut update, id) in updates.into_iter().zip(update_ids) {
            let checked = match records.iter().find(|record| record.id == update.id) {
                Some(record) => check_shared_update(view, record, &mut update),
                None => Err(SetError::not_found()),
            };
            match checked {
                Ok(()) => {
                    allowed.push(update);
                    allowed_ids.push(id);
                }
                Err(err) => {
                    response.not_updated.insert(id, err.to_json());
                }
            }
        }
        (updates, update_ids) = (allowed, allowed_ids);
    }
    let results = ctx.jmap.store.update_emails(ctx.account.id, updates).await?;
    for (id, result) in update_ids.into_iter().zip(results) {
        match result {
            Ok(()) => {
                response.updated.insert(id, Value::Null);
            }
            Err(err) => {
                response.not_updated.insert(id, SetError::from(err).to_json());
            }
        }
    }
    Ok(())
}

pub async fn apply_destroys(ctx: &Ctx<'_>, destroy: &[String], response: &mut SetResponse) -> MethodResult<()> {
    let mut numbers = Vec::new();
    let mut valid = Vec::new();
    for id in destroy {
        match ctx.parse_id('e', id) {
            Some(number) => {
                numbers.push(number);
                valid.push(id.clone());
            }
            None => {
                response.not_destroyed.insert(id.clone(), SetError::not_found().to_json());
            }
        }
    }
    if let Some(view) = &ctx.shared {
        // Destroying takes the email out of every mailbox: all of them have to be shared with the
        // caller for removing.
        let records = ctx.jmap.store.emails_by_ids(ctx.account.id, numbers.clone()).await?;
        let mut allowed = Vec::new();
        let mut allowed_ids = Vec::new();
        for (number, id) in numbers.into_iter().zip(valid) {
            match records.iter().find(|record| record.id == number) {
                Some(record) if !view.may_read_email(&record.mailbox_ids) => {
                    response.not_destroyed.insert(id, SetError::not_found().to_json());
                }
                Some(record) if record.mailbox_ids.iter().all(|mailbox| view.may(*mailbox, "te")) => {
                    allowed.push(number);
                    allowed_ids.push(id);
                }
                Some(_) => {
                    let err = SetError::new("forbidden", "you may not remove this message from every mailbox");
                    response.not_destroyed.insert(id, err.to_json());
                }
                None => {
                    response.not_destroyed.insert(id, SetError::not_found().to_json());
                }
            }
        }
        (numbers, valid) = (allowed, allowed_ids);
    }
    let results = ctx.jmap.store.destroy_emails(ctx.account.id, numbers).await?;
    for (id, result) in valid.into_iter().zip(results) {
        match result {
            Ok(()) => response.destroyed.push(id),
            Err(err) => {
                response.not_destroyed.insert(id, SetError::from(err).to_json());
            }
        }
    }
    Ok(())
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result: Result<uwumail_store::IngestedEmail, SetError> = async {
                let object = object
                    .as_object()
                    .ok_or_else(|| SetError::new("invalidProperties", "the email must be an object"))?;
                let mailboxes = mailbox_ids(ctx, object.get("mailboxIds"))?;
                let keywords = keywords(object.get("keywords"))?;
                check_shared_create(ctx, &mailboxes, &keywords)?;
                let received_at = received_at(object.get("receivedAt"))?;
                let mut blob_ids = Vec::new();
                collect_blob_ids(&Value::Object(object.clone()), &mut blob_ids);
                let mut loaded = HashMap::new();
                for blob_id in blob_ids {
                    let bytes = read_blob(ctx, &blob_id)
                        .await
                        .ok_or_else(|| SetError::new("blobNotFound", format!("blob {blob_id} not found")))?;
                    loaded.insert(blob_id, bytes);
                }
                let raw = email_json::build_message(object, &LoadedBlobs(loaded))
                    .map_err(|err| SetError::invalid_properties(&[err.property.as_str()], err.description))?;
                ctx.jmap
                    .store
                    .ingest(IngestRequest { account_id: ctx.account.id, raw, mailboxes, keywords, received_at })
                    .await
                    .map_err(SetError::from)
            }
            .await;
            match result {
                Ok(email) => {
                    ctx.created_ids.insert(creation_id.clone(), ids::email(email.id));
                    response.created.insert(creation_id.clone(), created_json(&email));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        apply_updates(ctx, update, &mut response).await?;
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        let ids: Vec<String> =
            destroy.iter().filter_map(Value::as_str).map(|id| ctx.resolve(id).unwrap_or(id).to_owned()).collect();
        apply_destroys(ctx, &ids, &mut response).await?;
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

pub async fn import(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let emails = args
        .get("emails")
        .and_then(Value::as_object)
        .ok_or_else(|| MethodError::invalid_arguments("emails is required"))?;
    let mut created = Map::new();
    let mut not_created = Map::new();
    for (creation_id, object) in emails {
        let result: Result<uwumail_store::IngestedEmail, SetError> = async {
            let blob_id = object
                .get("blobId")
                .and_then(Value::as_str)
                .ok_or_else(|| SetError::invalid_properties(&["blobId"], "blobId is required"))?;
            let raw = read_blob(ctx, blob_id)
                .await
                .ok_or_else(|| SetError::new("blobNotFound", format!("blob {blob_id} not found")))?;
            let mailboxes = mailbox_ids(ctx, object.get("mailboxIds"))?;
            let keywords = keywords(object.get("keywords"))?;
            check_shared_create(ctx, &mailboxes, &keywords)?;
            let received_at = received_at(object.get("receivedAt"))?;
            ctx.jmap
                .store
                .ingest(IngestRequest { account_id: ctx.account.id, raw, mailboxes, keywords, received_at })
                .await
                .map_err(|err| match err {
                    StoreError::QuotaExceeded => SetError::new("overQuota", "the mailbox is full"),
                    other => SetError::from(other),
                })
        }
        .await;
        match result {
            Ok(email) => {
                ctx.created_ids.insert(creation_id.clone(), ids::email(email.id));
                created.insert(creation_id.clone(), created_json(&email));
            }
            Err(err) => {
                not_created.insert(creation_id.clone(), err.to_json());
            }
        }
    }
    let new_state = ctx.state().await?;
    Ok(json!({
        "accountId": ctx.account_id(),
        "oldState": old_state,
        "newState": new_state,
        "created": if created.is_empty() { Value::Null } else { Value::Object(created) },
        "notCreated": if not_created.is_empty() { Value::Null } else { Value::Object(not_created) },
    }))
}

pub async fn parse(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let blob_ids: Vec<String> = args
        .get("blobIds")
        .and_then(Value::as_array)
        .ok_or_else(|| MethodError::invalid_arguments("blobIds is required"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let defaults: Vec<&str> = DEFAULT_PROPERTIES
        .iter()
        .copied()
        .filter(|p| !matches!(*p, "id" | "threadId" | "mailboxIds" | "keywords" | "receivedAt"))
        .collect();
    let properties = properties(args, "properties", &defaults)?.into_iter().filter(|p| p != "id").collect::<Vec<_>>();
    let body_properties = super::properties(args, "bodyProperties", DEFAULT_BODY_PROPERTIES)?;
    let options = BodyValueOptions {
        text: args.get("fetchTextBodyValues").and_then(Value::as_bool).unwrap_or(false),
        html: args.get("fetchHTMLBodyValues").and_then(Value::as_bool).unwrap_or(false),
        all: args.get("fetchAllBodyValues").and_then(Value::as_bool).unwrap_or(false),
        max_bytes: args.get("maxBodyValueBytes").and_then(Value::as_u64).unwrap_or(0) as usize,
    };
    let mut parsed = Map::new();
    let mut not_parsable = Vec::new();
    let mut not_found = Vec::new();
    for blob_id in blob_ids {
        let Some(raw) = read_blob(ctx, &blob_id).await else {
            not_found.push(blob_id);
            continue;
        };
        if mail_parser::MessageParser::default().parse(&raw).is_none() {
            not_parsable.push(blob_id);
            continue;
        }
        let hash = uwumail_store::BlobHash::of(&raw);
        parsed.insert(blob_id, email_json::to_json(None, Some(&raw), &hash, &properties, &body_properties, options));
    }
    Ok(json!({
        "accountId": ctx.account_id(),
        "parsed": if parsed.is_empty() { Value::Null } else { Value::Object(parsed) },
        "notParsable": if not_parsable.is_empty() { Value::Null } else { json!(not_parsable) },
        "notFound": if not_found.is_empty() { Value::Null } else { json!(not_found) },
    }))
}
