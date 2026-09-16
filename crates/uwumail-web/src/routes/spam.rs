//! The spam filter in My account and for admins: what the Bayes filter learned, and learning once
//! from mail that is already sorted into Junk or kept in the inbox.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};
use uwumail_store::{BAYES_FOLDER_LIMIT, BAYES_MIN_LEARNED, BAYES_WANTED_AFTER_SECS};

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

pub async fn account_overview(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let store = web.store();
    Ok(Json(json!({
        "bayes": {
            "enabled": web.smtp().spam_settings().bayes,
            "minimum": BAYES_MIN_LEARNED,
            "own": store.bayes_totals(Some(session.account.id)).await?,
            "server": store.bayes_totals(None).await?,
        },
    })))
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
