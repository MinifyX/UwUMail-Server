//! Built-in lists the server fetches itself: malware links and files from abuse.ch, spam subjects from
//! mailcow, and throwaway, freemail and link-shortener domains from Rspamd. Settings switch each of them on
//! and off; the abuse.ch lists also need the admin's own Auth-Key. Subscribed word lists are fetched here too.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::sync::watch;
use uwumail_store::{WORD_SOURCE_MAX_BYTES, WordSource, normalize_domain, parse_word_lines, word_regex};

use super::links;
use crate::config::FeedsConfig;
use crate::fetch::Fetched;
use crate::{Context, Smtp};

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
/// A list whose last fetch failed is tried again after this long.
const RETRY: i64 = HOUR;
/// How often the server looks whether a list is due.
const TICK: Duration = Duration::from_secs(10 * 60);
/// Subjects no list may match; an expression that does would sort ordinary mail into Junk.
const PLAIN_SUBJECTS: &[&str] = &[
    "Hallo",
    "Re: Termin morgen",
    "Ihre Bestellung ist unterwegs",
    "Einladung zum Elternabend",
    "Rechnung September",
    "Your order has shipped",
    "Meeting notes",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Links, one per line or in CSV, with offline ones left out.
    MalwareLinks,
    /// MD5 or SHA-256 hashes, one per line or in CSV.
    MalwareFiles,
    /// Regular expressions for subjects, like Rspamd's regexp maps.
    Subjects,
    /// Domain names, one per line.
    Domains,
}

/// A built-in list.
#[derive(Debug)]
pub struct Feed {
    pub key: &'static str,
    /// Who publishes it.
    pub source: &'static str,
    /// Where to read about it and its terms.
    pub page: &'static str,
    /// `{key}` stands for the abuse.ch Auth-Key.
    url: &'static str,
    pub needs_key: bool,
    /// How long a fetched list is good for.
    pub interval_secs: i64,
    max_bytes: usize,
    pub(crate) kind: Kind,
}

pub const FEEDS: &[Feed] = &[
    Feed {
        key: "urlhaus",
        source: "abuse.ch URLhaus",
        page: "https://urlhaus.abuse.ch/api/",
        url: "https://urlhaus-api.abuse.ch/v2/files/exports/{key}/recent.csv",
        needs_key: true,
        interval_secs: HOUR,
        max_bytes: 64 * 1024 * 1024,
        kind: Kind::MalwareLinks,
    },
    Feed {
        key: "malware_bazaar",
        source: "abuse.ch MalwareBazaar",
        page: "https://bazaar.abuse.ch/export/",
        url: "https://mb-api.abuse.ch/v2/files/exports/{key}/recent.csv",
        needs_key: true,
        interval_secs: HOUR,
        max_bytes: 64 * 1024 * 1024,
        kind: Kind::MalwareFiles,
    },
    Feed {
        key: "bad_subjects",
        source: "mailcow",
        page: "https://github.com/mailcow/mailcow-dockerized",
        url: "http://fuzzy.mailcow.email/bad-subject-regex.txt",
        needs_key: false,
        interval_secs: DAY,
        max_bytes: 1024 * 1024,
        kind: Kind::Subjects,
    },
    Feed {
        key: "disposable",
        source: "Rspamd",
        page: "https://rspamd.com/",
        url: "https://maps.rspamd.com/freemail/disposable.txt.zst",
        needs_key: false,
        interval_secs: DAY,
        max_bytes: 4 * 1024 * 1024,
        kind: Kind::Domains,
    },
    Feed {
        key: "freemail",
        source: "Rspamd",
        page: "https://rspamd.com/",
        url: "https://maps.rspamd.com/freemail/free.txt.zst",
        needs_key: false,
        interval_secs: DAY,
        max_bytes: 4 * 1024 * 1024,
        kind: Kind::Domains,
    },
    Feed {
        key: "redirectors",
        source: "Rspamd",
        page: "https://rspamd.com/",
        url: "https://maps.rspamd.com/rspamd/redirectors.inc.zst",
        needs_key: false,
        interval_secs: DAY,
        max_bytes: 4 * 1024 * 1024,
        kind: Kind::Domains,
    },
];

pub fn feed(key: &str) -> Option<&'static Feed> {
    FEEDS.iter().find(|feed| feed.key == key)
}

impl Feed {
    /// Whether the settings switch the list on, and it has what it needs.
    pub fn active(&self, config: &FeedsConfig) -> bool {
        let on = match self.key {
            "urlhaus" => config.urlhaus,
            "malware_bazaar" => config.malware_bazaar,
            "bad_subjects" => config.bad_subjects,
            "disposable" => config.disposable,
            "freemail" => config.freemail,
            "redirectors" => config.redirectors,
            _ => false,
        };
        on && (!self.needs_key || self.abuse_ch_key(config).is_some())
    }

    fn abuse_ch_key<'a>(&self, config: &'a FeedsConfig) -> Option<&'a str> {
        config.abuse_ch_key.as_deref().map(str::trim).filter(|key| !key.is_empty())
    }

    fn url(&self, config: &FeedsConfig) -> Option<String> {
        if !self.needs_key {
            return Some(self.url.to_owned());
        }
        let key = self.abuse_ch_key(config)?;
        // The key goes into the path; anything but plain characters would change where the request goes.
        key.chars().all(|c| c.is_ascii_alphanumeric()).then(|| self.url.replace("{key}", key))
    }
}

fn fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields.into_iter().map(|field| field.trim().to_owned()).collect()
}

/// A list's values from what was fetched. A list with nothing usable in it is an error, so a changed format
/// or an error page never empties what the filter knows.
pub(crate) fn parse(kind: Kind, text: &str) -> Result<Vec<String>, String> {
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#'));
    let mut values: Vec<String> = match kind {
        Kind::MalwareLinks => lines
            .map(fields)
            .filter(|fields| !fields.iter().any(|field| field.eq_ignore_ascii_case("offline")))
            .flat_map(|fields| fields.into_iter().filter_map(|field| links::normalized_url(&field)))
            .collect(),
        Kind::MalwareFiles => lines
            .flat_map(fields)
            .filter(|field| matches!(field.len(), 32 | 64) && field.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|field| field.to_ascii_lowercase())
            .collect(),
        Kind::Subjects => {
            let (patterns, _) = parse_word_lines(&lines.collect::<Vec<_>>().join("\n"));
            patterns
                .into_iter()
                .filter(|pattern| pattern.starts_with('/'))
                .filter(|pattern| {
                    word_regex(pattern)
                        .ok()
                        .and_then(|source| regex::Regex::new(&source).ok())
                        .is_some_and(|regex| !PLAIN_SUBJECTS.iter().any(|subject| regex.is_match(subject)))
                })
                .collect()
        }
        Kind::Domains => lines
            .filter_map(|line| line.split_whitespace().next())
            .filter_map(|domain| normalize_domain(domain).ok())
            .filter(|domain| domain.contains('.'))
            .collect(),
    };
    values.sort();
    values.dedup();
    if values.is_empty() {
        return Err("the list has no usable entries; maybe its format changed".into());
    }
    Ok(values)
}

/// The built-in lists' values, as the rules use them.
#[derive(Default)]
pub(crate) struct Sets {
    pub malware_links: HashSet<String>,
    pub malware_files: HashSet<String>,
    pub disposable: HashSet<String>,
    pub freemail: HashSet<String>,
    pub redirectors: HashSet<String>,
    /// Spam subjects, to count with the whole server's word lists.
    pub subjects: Vec<String>,
}

impl Sets {
    pub fn new(mut values: HashMap<String, Vec<String>>, config: &FeedsConfig) -> Sets {
        let mut take = |key: &str| -> Vec<String> {
            match feed(key) {
                Some(feed) if feed.active(config) => values.remove(key).unwrap_or_default(),
                _ => Vec::new(),
            }
        };
        Sets {
            malware_links: take("urlhaus").into_iter().collect(),
            malware_files: take("malware_bazaar").into_iter().collect(),
            disposable: take("disposable").into_iter().collect(),
            freemail: take("freemail").into_iter().collect(),
            redirectors: take("redirectors").into_iter().collect(),
            subjects: take("bad_subjects"),
        }
    }
}

/// `domain` or the nearest parent domain on a list.
pub(crate) fn listed<'a>(set: &HashSet<String>, domain: &'a str) -> Option<&'a str> {
    let mut rest = domain;
    loop {
        if set.contains(rest) {
            return Some(rest);
        }
        match rest.split_once('.') {
            Some((_, parent)) if parent.contains('.') => rest = parent,
            _ => return None,
        }
    }
}

/// A fingerprint of which lists count, so the compiled lists follow the settings.
pub(crate) fn active_mask(config: &FeedsConfig) -> u32 {
    FEEDS.iter().enumerate().filter(|(_, feed)| feed.active(config)).fold(0, |mask, (index, _)| mask | 1 << index)
}

/// Fetches a built-in list now. Returns how many values it holds, or why it failed.
pub(crate) async fn refresh_feed(ctx: &Context, feed: &Feed) -> Result<usize, String> {
    let config = ctx.live().spam.feeds.clone();
    let result: Result<Option<usize>, String> = async {
        let url = feed.url(&config).ok_or_else(|| "the abuse.ch Auth-Key is missing or invalid".to_owned())?;
        let validator = ctx.store.feed_validator(feed.key).await.map_err(|err| err.to_string())?;
        let shown = format!("the {} list", feed.source);
        let allow_http = url.starts_with("http://");
        match ctx.fetcher.get(&url, allow_http, validator.as_deref(), feed.max_bytes, &shown).await? {
            Fetched::Unchanged => {
                ctx.store.feed_fetched(feed.key, None).await.map_err(|err| err.to_string())?;
                Ok(None)
            }
            Fetched::Fresh { body, validator } => {
                let kind = feed.kind;
                let values = tokio::task::spawn_blocking(move || parse(kind, &String::from_utf8_lossy(&body)))
                    .await
                    .map_err(|err| err.to_string())??;
                let count = values.len();
                ctx.store.replace_feed(feed.key, values, validator).await.map_err(|err| err.to_string())?;
                Ok(Some(count))
            }
        }
    }
    .await;
    match result {
        Ok(Some(count)) => {
            tracing::info!(list = feed.key, entries = count, "fetched a built-in list");
            Ok(count)
        }
        Ok(None) => Ok(0),
        Err(error) => {
            tracing::warn!(list = feed.key, %error, "fetching a built-in list failed");
            let _ = ctx.store.feed_fetched(feed.key, Some(error.clone())).await;
            Err(error)
        }
    }
}

/// Fetches a subscribed word list now. Returns how many entries it brought, or why it failed.
pub(crate) async fn refresh_word_source(ctx: &Context, source: &WordSource) -> Result<usize, String> {
    let result: Result<Option<usize>, String> = async {
        let fetched =
            ctx.fetcher.get(&source.url, false, source.validator.as_deref(), WORD_SOURCE_MAX_BYTES, "the link").await?;
        match fetched {
            Fetched::Unchanged => Ok(None),
            Fetched::Fresh { body, validator } => {
                let text = String::from_utf8(body).map_err(|_| "the list is not UTF-8 text".to_owned())?;
                let (patterns, _) = tokio::task::spawn_blocking(move || parse_word_lines(&text))
                    .await
                    .map_err(|err| err.to_string())?;
                if patterns.is_empty() {
                    return Err("the list has no usable entries".to_owned());
                }
                let count = patterns.len().min(uwumail_store::WORD_SOURCE_ENTRY_LIMIT);
                ctx.store
                    .replace_word_source_entries(source.id, patterns, validator)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(Some(count))
            }
        }
    }
    .await;
    match result {
        Ok(count) => {
            if count.is_none() {
                let _ = ctx.store.word_source_fetched(source.id, None).await;
            }
            Ok(count.unwrap_or(source.entries as usize))
        }
        Err(error) => {
            let _ = ctx.store.word_source_fetched(source.id, Some(error.clone())).await;
            Err(error)
        }
    }
}

async fn update_due(ctx: &Context) {
    let config = ctx.live().spam.feeds.clone();
    let now = crate::now();
    let states: HashMap<String, (Option<i64>, bool)> = match ctx.store.feed_states().await {
        Ok(states) => states.into_iter().map(|state| (state.key, (state.fetched_at, state.error.is_some()))).collect(),
        Err(err) => {
            tracing::warn!(%err, "reading the state of the built-in lists failed");
            return;
        }
    };
    for feed in FEEDS.iter().filter(|feed| feed.active(&config)) {
        let due = match states.get(feed.key) {
            Some((Some(at), failed)) => now - at >= if *failed { RETRY } else { feed.interval_secs },
            _ => true,
        };
        if due {
            let _ = refresh_feed(ctx, feed).await;
        }
    }
    match ctx.store.word_sources_due(now - DAY).await {
        Ok(sources) => {
            for source in sources {
                if let Err(error) = refresh_word_source(ctx, &source).await {
                    tracing::info!(source = source.id, %error, "fetching a subscribed word list failed");
                }
            }
        }
        Err(err) => tracing::warn!(%err, "reading the subscribed word lists failed"),
    }
}

/// Keeps the built-in lists and subscribed word lists fresh.
pub async fn run_list_updates(smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    loop {
        update_due(&ctx).await;
        // Entries that were meant for a while only go once their time is up.
        match ctx.store.remove_expired_rules().await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "sender and word list entries ran out"),
            Err(err) => tracing::warn!(%err, "removing list entries that ran out failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_read_in_their_formats() {
        let urlhaus = "# URLhaus\n# id,dateadded,url,url_status,last_online,threat\n\
            \"1\",\"2026-09-17\",\"http://198.51.100.7:8080/bins/x.sh\",\"online\",\"\",\"malware_download\"\n\
            \"2\",\"2026-09-17\",\"https://Files.Example.org/a,b.exe\",\"online\",\"\",\"malware_download\"\n\
            \"3\",\"2026-09-16\",\"http://gone.example/y\",\"offline\",\"\",\"malware_download\"\n";
        assert_eq!(
            parse(Kind::MalwareLinks, urlhaus).unwrap(),
            ["http://198.51.100.7:8080/bins/x.sh", "https://files.example.org/a,b.exe"]
        );
        assert_eq!(parse(Kind::MalwareLinks, "https://plain.example/x\n").unwrap(), ["https://plain.example/x"]);

        let md5 = "# MalwareBazaar\n44d88612fea8a8f36de82e1278abb02f\n";
        assert_eq!(parse(Kind::MalwareFiles, md5).unwrap(), ["44d88612fea8a8f36de82e1278abb02f"]);
        let csv = "# \"first_seen_utc\",\"sha256_hash\",\"md5_hash\"\n\
            \"2026-09-17 05:00:00\",\"275A021BBFB6489E54D471899F7DB9D1663FC695EC2FE2A2C4538AABF651FD0F\",\"44d88612fea8a8f36de82e1278abb02f\"\n";
        assert_eq!(parse(Kind::MalwareFiles, csv).unwrap().len(), 2);

        let subjects = "/[0-9]+.+Mio.+Rekord.+Jackpot/i\n/.*/i\n/Hallo/\n/(?=x)/\n";
        assert_eq!(parse(Kind::Subjects, subjects).unwrap(), ["/[0-9]+.+Mio.+Rekord.+Jackpot/i"]);

        assert_eq!(
            parse(Kind::Domains, "0815.example\nTrashmail.EXAMPLE\ncom\n").unwrap(),
            ["0815.example", "trashmail.example"]
        );
        assert!(parse(Kind::Domains, "<html>error</html>").is_err(), "an error page empties nothing");
    }

    /// Fetches the real lists that need no key: `cargo test -p uwumail-smtp --lib real_lists -- --ignored`.
    #[tokio::test]
    #[ignore = "reaches the internet"]
    async fn real_lists_can_be_fetched_and_read() {
        let fetcher = crate::fetch::Fetcher::new();
        for key in ["bad_subjects", "disposable", "freemail", "redirectors"] {
            let feed = feed(key).unwrap();
            let url = feed.url(&FeedsConfig::default()).unwrap();
            let http = url.starts_with("http://");
            let Fetched::Fresh { body, validator } = fetcher.get(&url, http, None, feed.max_bytes, key).await.unwrap()
            else {
                panic!("{key}: nothing fetched")
            };
            let values = parse(feed.kind, &String::from_utf8_lossy(&body)).unwrap();
            assert!(values.len() > 50, "{key}: only {} entries", values.len());
            let unchanged = match &validator {
                Some(validator) => {
                    fetcher.get(&url, http, Some(validator), feed.max_bytes, key).await.unwrap() == Fetched::Unchanged
                }
                None => false,
            };
            eprintln!(
                "{key}: {} entries, validator {validator:?}, unchanged on the second fetch: {unchanged}",
                values.len()
            );
        }
    }

    #[test]
    fn switches_and_the_key_decide_what_counts() {
        let mut config = FeedsConfig::default();
        let urlhaus = feed("urlhaus").unwrap();
        assert!(!urlhaus.active(&config), "no key yet");
        assert!(feed("disposable").unwrap().active(&config));
        config.abuse_ch_key = Some("abc123".into());
        assert!(urlhaus.active(&config));
        assert_eq!(urlhaus.url(&config).unwrap(), "https://urlhaus-api.abuse.ch/v2/files/exports/abc123/recent.csv");
        config.abuse_ch_key = Some("../../other".into());
        assert_eq!(urlhaus.url(&config), None, "a key cannot change the path");
        config.disposable = false;
        assert!(!feed("disposable").unwrap().active(&config));

        let set: HashSet<String> = ["trashmail.example".to_owned()].into();
        assert_eq!(listed(&set, "eu.trashmail.example"), Some("trashmail.example"));
        assert_eq!(listed(&set, "trashmail.example.evil.example"), None);
    }
}
