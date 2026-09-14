//! EmailSubmission/get, /query and /set (RFC 8621, section 7).

use serde_json::{Map, Value, json};
use uwumail_smtp::{Submission, SubmissionRecipient, SubmitError};
use uwumail_store::SubmissionRecord;

use super::email::{apply_destroys, apply_updates};
use super::{Ctx, Outputs, SetResponse, check_set_size, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};
use crate::{dates, ids};

const DEFAULTS: &[&str] = &[
    "id",
    "identityId",
    "emailId",
    "threadId",
    "envelope",
    "sendAt",
    "undoStatus",
    "deliveryStatus",
    "dsnBlobIds",
    "mdnBlobIds",
];

fn to_json(record: &SubmissionRecord) -> Map<String, Value> {
    let envelope: Value = serde_json::from_str(&record.envelope).unwrap_or(Value::Null);
    let Value::Object(map) = json!({
        "id": ids::submission(record.id),
        "identityId": ids::identity(record.identity_id),
        "emailId": ids::email(record.email_id),
        "threadId": ids::thread(record.thread_id),
        "envelope": envelope,
        "sendAt": dates::format(record.send_at),
        "undoStatus": record.undo_status,
        "deliveryStatus": null,
        "dsnBlobIds": [],
        "mdnBlobIds": [],
    }) else {
        unreachable!("object literal")
    };
    map
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let requested = get_ids(args)?;
    let numbers = requested.as_ref().map(|list| list.iter().filter_map(|id| ctx.parse_id('s', id)).collect());
    let records = ctx.jmap.store.submissions(ctx.account.id, numbers).await?;
    let list: Vec<Value> = records.iter().map(|r| pick(to_json(r), &properties)).collect();
    let not_found: Vec<String> = requested
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !ctx.parse_id('s', id).is_some_and(|n| records.iter().any(|r| r.id == n)))
        .collect();
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let mut records = ctx.jmap.store.submissions(ctx.account.id, None).await?;
    if let Some(filter) = args.get("filter").and_then(Value::as_object) {
        for (key, value) in filter {
            let list = |prefix: char| -> Vec<i64> {
                value
                    .as_array()
                    .map(|l| l.iter().filter_map(|v| v.as_str().and_then(|v| ctx.parse_id(prefix, v))).collect())
                    .unwrap_or_default()
            };
            match key.as_str() {
                "identityIds" => {
                    let wanted = list('i');
                    records.retain(|r| wanted.contains(&r.identity_id));
                }
                "emailIds" => {
                    let wanted = list('e');
                    records.retain(|r| wanted.contains(&r.email_id));
                }
                "threadIds" => {
                    let wanted = list('t');
                    records.retain(|r| wanted.contains(&r.thread_id));
                }
                "undoStatus" => {
                    let wanted = value.as_str().unwrap_or_default();
                    records.retain(|r| r.undo_status == wanted);
                }
                "before" | "after" => {
                    let date =
                        value.as_str().and_then(dates::parse).ok_or_else(|| MethodError::kind("unsupportedFilter"))?;
                    records.retain(|r| if key == "before" { r.send_at < date } else { r.send_at >= date });
                }
                other => return Err(MethodError::new("unsupportedFilter", format!("unknown filter {other}"))),
            }
        }
    }
    let ids: Vec<String> = records.iter().map(|r| ids::submission(r.id)).collect();
    Ok(
        json!({ "accountId": ctx.account_id(), "queryState": state, "canCalculateChanges": false, "position": 0, "total": ids.len(), "ids": ids }),
    )
}

fn envelope_addresses(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()
        .map(|list| list.iter().filter_map(|a| a.get("email").and_then(Value::as_str).map(str::to_owned)).collect())
}

async fn create_one(ctx: &Ctx<'_>, object: &Map<String, Value>) -> Result<(i64, i64, String), SetError> {
    let identity_id = object
        .get("identityId")
        .and_then(Value::as_str)
        .and_then(|id| ctx.parse_id('i', id))
        .ok_or_else(|| SetError::invalid_properties(&["identityId"], "identityId must name an identity"))?;
    let identity = ctx
        .jmap
        .store
        .identities(ctx.account.id)
        .await?
        .into_iter()
        .find(|i| i.id == identity_id)
        .ok_or_else(|| SetError::invalid_properties(&["identityId"], "the identity does not exist"))?;
    let email_id = object
        .get("emailId")
        .and_then(Value::as_str)
        .and_then(|id| ctx.parse_id('e', id))
        .ok_or_else(|| SetError::invalid_properties(&["emailId"], "emailId must name an email"))?;
    let record = ctx
        .jmap
        .store
        .email(ctx.account.id, email_id)
        .await
        .map_err(|_| SetError::invalid_properties(&["emailId"], "the email does not exist"))?;

    let envelope = object.get("envelope").filter(|e| !e.is_null());
    let mail_from = envelope
        .and_then(|e| e.pointer("/mailFrom/email"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| identity.email.clone());
    let rcpt_to: Vec<String> = match envelope.and_then(|e| e.get("rcptTo")) {
        Some(list) => envelope_addresses(list).unwrap_or_default(),
        None => {
            let mut all: Vec<String> =
                record.to.iter().chain(&record.cc).chain(&record.bcc).map(|a| a.email.to_lowercase()).collect();
            all.dedup();
            all
        }
    };
    if rcpt_to.is_empty() {
        return Err(SetError::new("noRecipients", "the email has no recipients"));
    }

    let raw = ctx.jmap.store.blob(&record.blob).await?;
    let submission = Submission {
        account: ctx.account.clone(),
        mail_from: mail_from.clone(),
        recipients: rcpt_to.iter().cloned().map(SubmissionRecipient::new).collect(),
        raw,
        env_id: None,
        trace: None,
    };
    let submitted = ctx.jmap.smtp.submit(submission).await.map_err(|err| match err {
        SubmitError::NoFrom => SetError::new("invalidEmail", "the email has no From header"),
        SubmitError::ForbiddenFrom(address) if address == mail_from => {
            SetError::new("forbiddenMailFrom", format!("you are not allowed to send as {address}"))
        }
        SubmitError::ForbiddenFrom(address) => {
            SetError::new("forbiddenFrom", format!("you are not allowed to send as {address}"))
        }
        SubmitError::NoRecipients => SetError::new("noRecipients", "the email has no recipients"),
        SubmitError::InvalidRecipient(address) => {
            SetError::new("invalidRecipients", format!("{address} is not a valid address"))
        }
        SubmitError::NobodyAccepted => SetError::new("forbiddenToSend", "no recipient could take the message"),
        SubmitError::Queue(err) => SetError::from(err),
    })?;
    let envelope_json = json!({
        "mailFrom": { "email": mail_from, "parameters": null },
        "rcptTo": rcpt_to.iter().map(|email| json!({ "email": email, "parameters": null })).collect::<Vec<_>>(),
    })
    .to_string();
    let id = ctx
        .jmap
        .store
        .record_submission(
            ctx.account.id,
            identity_id,
            email_id,
            record.thread_id,
            envelope_json,
            submitted.queue_message_id,
        )
        .await?;
    Ok((id, record.thread_id, ids::email(email_id)))
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Outputs> {
    check_set_size(args)?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    // Submission id (as the client names it) -> email id, for onSuccess* arguments.
    let mut succeeded: Vec<(String, String)> = Vec::new();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in create {
            let result = match object.as_object() {
                Some(object) => create_one(ctx, object).await,
                None => Err(SetError::new("invalidProperties", "the submission must be an object")),
            };
            match result {
                Ok((id, thread_id, email_id)) => {
                    let jmap_id = ids::submission(id);
                    ctx.created_ids.insert(creation_id.clone(), jmap_id.clone());
                    response.created.insert(
                        creation_id.clone(),
                        json!({ "id": jmap_id, "threadId": ids::thread(thread_id), "undoStatus": "final", "sendAt": dates::format(uwumail_now()) }),
                    );
                    succeeded.push((format!("#{creation_id}"), email_id.clone()));
                    succeeded.push((jmap_id, email_id));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }

    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for id in update.keys() {
            response
                .not_updated
                .insert(id.clone(), SetError::new("cannotUnsend", "the message was already sent").to_json());
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let result = match ctx.parse_id('s', id) {
                Some(number) => ctx.jmap.store.destroy_submission(ctx.account.id, number).await.map_err(SetError::from),
                None => Err(SetError::not_found()),
            };
            match result {
                Ok(()) => response.destroyed.push(id.to_owned()),
                Err(err) => {
                    response.not_destroyed.insert(id.to_owned(), err.to_json());
                }
            }
        }
    }

    let new_state = ctx.state().await?;
    let mut outputs = vec![("EmailSubmission/set".to_owned(), response.finish(ctx.account_id(), old_state, new_state))];

    let on_update = args.get("onSuccessUpdateEmail").and_then(Value::as_object);
    let on_destroy = args.get("onSuccessDestroyEmail").and_then(Value::as_array);
    if on_update.is_some() || on_destroy.is_some() {
        let email_old_state = ctx.state().await?;
        let mut email_response = SetResponse::default();
        if let Some(on_update) = on_update {
            let mut updates = Map::new();
            for (submission, patch) in on_update {
                if let Some((_, email_id)) = succeeded.iter().find(|(s, _)| s == submission) {
                    updates.insert(email_id.clone(), patch.clone());
                }
            }
            apply_updates(ctx, &updates, &mut email_response).await?;
        }
        if let Some(on_destroy) = on_destroy {
            let emails: Vec<String> = on_destroy
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|submission| succeeded.iter().find(|(s, _)| s == submission).map(|(_, e)| e.clone()))
                .collect();
            apply_destroys(ctx, &emails, &mut email_response).await?;
        }
        let email_new_state = ctx.state().await?;
        outputs
            .push(("Email/set".to_owned(), email_response.finish(ctx.account_id(), email_old_state, email_new_state)));
    }
    Ok(outputs)
}

fn uwumail_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
