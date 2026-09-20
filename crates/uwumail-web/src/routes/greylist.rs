//! "Waiting mail" in My account: what greylisting is currently holding back, and what its recipient
//! decides to do with it.
//!
//! Everything here answers for the logged-in account and nothing else. There is deliberately no
//! admin route to any of it: unlike the spam history, these rows hold whole messages, and a message
//! belongs to the person it was addressed to.
//!
//! The list itself never carries a body, a link or an attachment — only who wrote and what about.
//! Whoever wants to read a waiting message delivers it to their own mailbox first and reads it
//! there, where a mail client does the usual about remote images and links, rather than in a
//! settings page that has none of those defences.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{
    IngestRequest, ListScope, MailboxRole, MailboxTarget, NewSenderListEntry, SenderList, Settled, StoreError,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// What someone can do with a message that is waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decide {
    /// Put it in the inbox, and never greylist this sender for me again.
    AllowDeliver,
    /// Put it in the inbox, this once.
    Deliver,
    /// Throw it away.
    Discard,
    /// Throw it away and let the filter learn from it.
    DiscardSpam,
}

#[derive(Deserialize)]
pub struct Decision {
    action: Decide,
}

async fn view(web: &Web, session: &Session) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "enabled": web.smtp().spam_settings().greylist_hold,
        "waiting": web.store().greylist_holds(session.account.id).await?,
    })))
}

/// What greylisting is holding back for this person right now.
pub async fn waiting(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    view(&web, &session).await
}

/// Acts on one waiting message.
///
/// The order matters. Delivering stores the message in the mailbox first and settles the row only
/// afterwards, because settling is what lets go of the kept copy — the other way round a cleanup
/// running in between could take the message out from under the delivery. Learning from a discarded
/// message keeps the copy instead, until the row expires: the spam filter learns off its own queue,
/// some time after this request is long finished, and would otherwise find nothing left to learn.
pub async fn decide(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<Value>> {
    let store = web.store();
    let account = session.account.id;
    let held = store
        .greylist_hold_message(account, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("this waiting message".into()))?;

    match decision.action {
        Decide::AllowDeliver | Decide::Deliver => {
            let request = IngestRequest {
                account_id: account,
                raw: held.message,
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                // It arrived when it arrived, not when the decision was made.
                received_at: Some(held.received_at),
            };
            store.ingest(request).await?;
            store.settle_greylist_hold(account, id, Settled::Delivered, false).await?;
            if decision.action == Decide::AllowDeliver {
                allow_sender(&web, &session, &held.envelope_from).await?;
            }
            tracing::info!(
                login = %session.account.login,
                sender = %held.envelope_from,
                allowed = decision.action == Decide::AllowDeliver,
                "delivered a waiting message"
            );
        }
        Decide::Discard | Decide::DiscardSpam => {
            let learn = decision.action == Decide::DiscardSpam;
            if learn {
                // For the whole server and for them, the same as marking mail as spam by hand.
                store.queue_bayes_learning(held.hash.clone(), None, true).await?;
                store.queue_bayes_learning(held.hash, Some(account), true).await?;
            }
            store.settle_greylist_hold(account, id, Settled::Discarded, learn).await?;
            tracing::info!(
                login = %session.account.login,
                sender = %held.envelope_from,
                learn,
                "discarded a waiting message"
            );
        }
    }
    view(&web, &session).await
}

/// Puts the sender on this person's own allowed list, so the next message from them is not held
/// back. Someone who is already on it is exactly where they wanted them, so that is not an error.
async fn allow_sender(web: &Web, session: &Session, sender: &str) -> ApiResult<()> {
    let entry = NewSenderListEntry {
        scope: ListScope::Account(session.account.id),
        list: SenderList::Allow,
        kind: None,
        value: sender.to_owned(),
        note: String::new(),
        created_by: session.account.login.clone(),
    };
    match web.store().add_sender_list_entry(entry).await {
        Ok(_) | Err(StoreError::Rule { code: "senderListed", .. }) => Ok(()),
        Err(err) => Err(err.into()),
    }
}
