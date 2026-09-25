//! Email/copy (RFC 8620, section 5.4; RFC 8621, section 4.7): copies emails from another account
//! this login can read into this one, optionally destroying the originals.
//!
//! Which other accounts a login can read is [`Ctx::readable_account`]; today that is none, so every
//! copy ends in `fromAccountNotFound`, as the RFC asks. Once accounts are shared, copying works
//! without changes here.

use serde_json::{Map, Value, json};
use uwumail_store::{IngestRequest, StoreError};

use super::email::{created_json, keywords, mailbox_ids, received_at};
use super::{Ctx, Outputs, SetResponse};
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

/// The only properties a copy may set; everything else comes from the original.
const OVERRIDABLE: &[&str] = &["id", "mailboxIds", "keywords", "receivedAt"];

pub async fn copy(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Outputs> {
    let from = args
        .get("fromAccountId")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("fromAccountId is required"))?;
    if from == ctx.account_id() {
        return Err(MethodError::invalid_arguments("fromAccountId must be another account than accountId"));
    }
    let from_account = ctx.readable_account(from).ok_or_else(|| MethodError::kind("fromAccountNotFound"))?;
    let create = match args.get("create") {
        Some(Value::Object(create)) => create.clone(),
        _ => return Err(MethodError::invalid_arguments("create must be an object")),
    };
    super::check_set_size(args)?;

    let from_state = ctx.jmap.store.account_modseq(from_account).await?.to_string();
    if let Some(expected) = args.get("ifFromInState").and_then(Value::as_str)
        && expected != from_state
    {
        return Err(MethodError::kind("stateMismatch"));
    }
    let old_state = ctx.state().await?;
    super::if_in_state(args, &old_state)?;

    let (created, not_created, copied) = copy_emails(ctx, from_account, &create).await?;
    let new_state = ctx.state().await?;
    let map_or_null = |map: Map<String, Value>| if map.is_empty() { Value::Null } else { Value::Object(map) };
    let mut outputs = vec![(
        "Email/copy".to_owned(),
        json!({
            "fromAccountId": from,
            "accountId": ctx.account_id(),
            "oldState": old_state,
            "newState": new_state,
            "created": map_or_null(created),
            "notCreated": map_or_null(not_created),
        }),
    )];

    if args.get("onSuccessDestroyOriginal").and_then(Value::as_bool).unwrap_or(false) {
        let old_from_state = ctx.jmap.store.account_modseq(from_account).await?.to_string();
        let mut response = SetResponse::default();
        match args.get("destroyFromIfInState").and_then(Value::as_str) {
            Some(expected) if expected != old_from_state => {
                outputs.push(("error".to_owned(), MethodError::kind("stateMismatch").to_json()));
                return Ok(outputs);
            }
            _ => {}
        }
        let results = ctx.jmap.store.destroy_emails(from_account, copied.iter().map(|(number, _)| *number).collect()).await?;
        for ((_, id), result) in copied.into_iter().zip(results) {
            match result {
                Ok(()) => response.destroyed.push(id),
                Err(err) => {
                    response.not_destroyed.insert(id, SetError::from(err).to_json());
                }
            }
        }
        let new_from_state = ctx.jmap.store.account_modseq(from_account).await?.to_string();
        outputs.push(("Email/set".to_owned(), response.finish(from.to_owned(), old_from_state, new_from_state)));
    }
    Ok(outputs)
}

/// What a copy produced: `created` and `notCreated` by creation id, and the originals that were
/// copied (number and JMAP id), for `onSuccessDestroyOriginal`.
type Copied = (Map<String, Value>, Map<String, Value>, Vec<(i64, String)>);

/// Copies each email of `create` from `from_account` into the login's own account.
pub(super) async fn copy_emails(ctx: &mut Ctx<'_>, from_account: i64, create: &Map<String, Value>) -> MethodResult<Copied> {
    let mut created = Map::new();
    let mut not_created = Map::new();
    let mut copied = Vec::new();
    for (creation_id, object) in create {
        let result: Result<(uwumail_store::IngestedEmail, i64, String), SetError> = async {
            let object =
                object.as_object().ok_or_else(|| SetError::new("invalidProperties", "the email must be an object"))?;
            if let Some(other) = object.keys().find(|key| !OVERRIDABLE.contains(&key.as_str())) {
                return Err(SetError::invalid_properties(&[other.as_str()], format!("{other} cannot be set in a copy")));
            }
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| SetError::invalid_properties(&["id"], "id names the email to copy"))?;
            let number = ids::parse('e', id).ok_or_else(SetError::not_found)?;
            let original = ctx.jmap.store.email(from_account, number).await.map_err(|err| match err {
                StoreError::NotFound(_) => SetError::not_found(),
                other => SetError::from(other),
            })?;
            let mailboxes = mailbox_ids(ctx, object.get("mailboxIds"))?;
            // Keywords and the time it arrived stay those of the original unless the copy says.
            let keywords = match object.get("keywords") {
                Some(value) => keywords(Some(value))?,
                None => original.keywords.clone(),
            };
            let received_at = match object.get("receivedAt") {
                Some(value) => received_at(Some(value))?,
                None => Some(original.received_at),
            };
            let raw = ctx.jmap.store.blob(&original.blob).await?;
            let email = ctx
                .jmap
                .store
                .ingest(IngestRequest { account_id: ctx.account.id, raw, mailboxes, keywords, received_at })
                .await
                .map_err(|err| match err {
                    StoreError::QuotaExceeded => SetError::new("overQuota", "the mailbox is full"),
                    other => SetError::from(other),
                })?;
            Ok((email, number, id.to_owned()))
        }
        .await;
        match result {
            Ok((email, number, id)) => {
                ctx.created_ids.insert(creation_id.clone(), ids::email(email.id));
                created.insert(creation_id.clone(), created_json(&email));
                copied.push((number, id));
            }
            Err(err) => {
                not_created.insert(creation_id.clone(), err.to_json());
            }
        }
    }
    Ok((created, not_created, copied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Jmap;
    use uwumail_smtp::{DeliveryConfig, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
    use uwumail_store::{MailboxRole, MailboxTarget, NewAccount, Role, Store};

    /// The copying itself, between two accounts, as it will run once one is shared with the other.
    #[tokio::test(flavor = "multi_thread")]
    async fn copies_between_accounts_and_keeps_what_the_copy_does_not_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let mut accounts = Vec::new();
        for user in ["mini", "nyu"] {
            let account = store
                .create_account(NewAccount {
                    address: format!("{user}@example.org"),
                    display_name: user.into(),
                    password: Some("katzenpfote-123".into()),
                    role: Role::User,
                    quota_bytes: 0,
                    protocols: None,
                })
                .await
                .unwrap();
            accounts.push(account);
        }
        let settings = SmtpSettings {
            hostname: "mail.example.org".into(),
            smtp: SmtpConfig::default(),
            spam: Default::default(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        };
        let jmap = Jmap::new(Smtp::new(store.clone(), settings).unwrap());
        let original = store
            .ingest(IngestRequest {
                account_id: accounts[0].id,
                raw: b"From: a@example.net\r\nSubject: Shared\r\n\r\nHello\r\n".to_vec(),
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec!["$seen".into()],
                received_at: Some(1_700_000_000),
            })
            .await
            .unwrap();
        let archive = store
            .mailboxes(accounts[1].id)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.role == Some(MailboxRole::Archive))
            .unwrap();

        let mut ctx = Ctx::new(&jmap.inner, accounts[1].clone(), vec![], Default::default());
        let create = json!({
            "c1": { "id": ids::email(original.id), "mailboxIds": { ids::mailbox(archive.id): true } },
            "c2": { "id": ids::email(original.id), "mailboxIds": { ids::mailbox(archive.id): true }, "subject": "no" },
            "c3": { "id": "e999999", "mailboxIds": { ids::mailbox(archive.id): true } },
        });
        let (created, not_created, copied) =
            copy_emails(&mut ctx, accounts[0].id, create.as_object().unwrap()).await.unwrap();
        assert_eq!(not_created["c2"]["type"], "invalidProperties");
        assert_eq!(not_created["c3"]["type"], "notFound");
        assert_eq!(copied, vec![(original.id, ids::email(original.id))]);
        let copy_id = ids::parse('e', created["c1"]["id"].as_str().unwrap()).unwrap();
        let copy = store.email(accounts[1].id, copy_id).await.unwrap();
        assert_eq!(copy.subject, "Shared");
        assert_eq!(copy.keywords, vec!["$seen".to_owned()]);
        assert_eq!(copy.received_at, 1_700_000_000);
        assert_eq!(copy.mailbox_ids, vec![archive.id]);
        assert_eq!(ctx.created_ids["c1"], created["c1"]["id"]);
    }
}
