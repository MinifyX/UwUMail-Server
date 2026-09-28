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

/// The lists last compiled, and the lock the next compile runs under.
#[derive(Default)]
pub(crate) struct Cache {
    current: std::sync::RwLock<Option<Arc<Lists>>>,
    compiling: tokio::sync::Mutex<()>,
}

impl Cache {
    fn get(&self) -> Option<Arc<Lists>> {
        self.current.read().expect("lists poisoned").clone()
    }
}

/// The lists as they are now.
///
/// A change compiles them again, one compile at a time and without holding up the messages: while
/// it runs, they are judged by the lists as they were, and only a message that finds none at all
/// waits. The compile takes over every scope that did not change (security-audit-0.16.0 SMTP-7).
pub(crate) async fn current(ctx: &Context) -> Arc<Lists> {
    let config = ctx.live().spam.feeds.clone();
    let cache = &ctx.lists;
    let versions = match (ctx.store.word_lists_version().await, ctx.store.feeds_version().await) {
        (Ok(words), Ok(feeds)) => (words, feeds),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "reading the version of the lists failed");
            return cache.get().unwrap_or_default();
        }
    };
    let key = (versions.0, versions.1, feeds::active_mask(&config));
    let cached = cache.get();
    if let Some(lists) = cached.as_ref().filter(|lists| lists.key == key) {
        return lists.clone();
    }
    let _compiling = match (cache.compiling.try_lock(), cached) {
        (Ok(guard), _) => guard,
        // Another message is compiling them already; this one goes by the lists as they were.
        (Err(_), Some(stale)) => return stale,
        (Err(_), None) => cache.compiling.lock().await,
    };
    let previous = cache.get();
    if let Some(lists) = previous.as_ref().filter(|lists| lists.key == key) {
        return lists.clone();
    }
    let (words, values) = match (ctx.store.compiled_words().await, ctx.store.feed_values().await) {
        (Ok(words), Ok(values)) => (words, values),
        (Err(err), _) | (_, Err(err)) => {
            tracing::warn!(%err, "reading the lists failed");
            return previous.unwrap_or_default();
        }
    };
    let compiled = tokio::task::spawn_blocking(move || {
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
        let empty = words::Compiled::default();
        let before = previous.as_ref().map_or(&empty, |lists| &lists.words);
        Arc::new(Lists { key, words: words::Compiled::rebuild(before, words), feeds })
    })
    .await;
    let Ok(lists) = compiled else {
        return cache.get().unwrap_or_default();
    };
    tracing::debug!(?key, "compiled the word lists and built-in lists");
    *cache.current.write().expect("lists poisoned") = Some(lists.clone());
    lists
}
