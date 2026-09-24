//! The word lists and built-in lists as the filter uses them. They are compiled again when a list changed or
//! the settings switched a built-in list on or off, and otherwise shared by every message.

use std::sync::Arc;

use uwumail_store::{CompiledWord, ListScope, WORD_POINTS};

use super::{feeds, words};
use crate::Context;

#[derive(Default)]
pub(crate) struct Lists {
    /// The word lists' version, the built-in lists' version and which built-in lists count.
    key: (i64, i64, u32),
    pub words: words::Compiled,
    pub feeds: feeds::Sets,
}

/// The lists as they are now.
pub(crate) async fn current(ctx: &Context) -> Arc<Lists> {
    let config = ctx.live().spam.feeds.clone();
    let mut cached = ctx.lists.lock().await;
    let versions = match (ctx.store.word_lists_version().await, ctx.store.feeds_version().await) {
        (Ok(words), Ok(feeds)) => (words, feeds),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "reading the version of the lists failed");
            return cached.clone().unwrap_or_default();
        }
    };
    let key = (versions.0, versions.1, feeds::active_mask(&config));
    if let Some(lists) = cached.as_ref().filter(|lists| lists.key == key) {
        return lists.clone();
    }
    let (words, values) = match (ctx.store.compiled_words().await, ctx.store.feed_values().await) {
        (Ok(words), Ok(values)) => (words, values),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "reading the lists failed");
            return cached.clone().unwrap_or_default();
        }
    };
    let lists = tokio::task::spawn_blocking(move || {
        let feeds = feeds::Sets::new(values, &config);
        let mut words = words;
        // Spam subjects from a built-in list count like the whole server's own subject entries.
        words.extend(feeds.subjects.iter().map(|pattern| CompiledWord {
            id: 0,
            scope: ListScope::Server,
            domain: None,
            pattern: pattern.clone(),
            points: WORD_POINTS,
            subject_only: true,
        }));
        Arc::new(Lists { key, words: words::Compiled::new(words), feeds })
    })
    .await
    .unwrap_or_default();
    tracing::debug!(?key, "compiled the word lists and built-in lists");
    *cached = Some(lists.clone());
    lists
}
