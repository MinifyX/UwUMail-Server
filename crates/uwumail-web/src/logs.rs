//! The newest server log lines, kept in memory so admins can read them in the portal.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::loki::Loki;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    /// This server.
    Server,
    /// The UwUMail Gateway in front of it, which hands its lines over through the tunnel.
    Gateway,
}

impl LogSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LogSource::Server => "server",
            LogSource::Gateway => "gateway",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    /// Increases with every line, for reading only what is new.
    pub seq: u64,
    /// Milliseconds since 1970.
    pub at: u64,
    pub source: LogSource,
    /// "error", "warn", "info", "debug" or "trace".
    pub level: &'static str,
    pub target: String,
    pub message: String,
    pub fields: Vec<(String, String)>,
}

pub struct LogBuffer {
    capacity: usize,
    inner: Mutex<(VecDeque<LogLine>, u64)>,
    /// Where every line also goes, once the server sends its logs on.
    loki: OnceLock<Arc<Loki>>,
}

fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

/// 0 for errors up to 4 for trace, so "at least warn" is `rank <= 1`.
pub fn level_rank(name: &str) -> u8 {
    match name {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        _ => 4,
    }
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Arc<LogBuffer> {
        Arc::new(LogBuffer {
            capacity,
            inner: Mutex::new((VecDeque::with_capacity(capacity), 1)),
            loki: OnceLock::new(),
        })
    }

    /// A tracing layer that copies every event into this buffer.
    pub fn layer(self: &Arc<Self>) -> LogLayer {
        LogLayer(self.clone())
    }

    /// Sends every line from now on to `loki` as well.
    pub fn forward_to(&self, loki: Arc<Loki>) {
        let _ = self.loki.set(loki);
    }

    /// Keeps a line the gateway logged. `level` is one of the level names; anything else counts as info.
    pub fn record_gateway(&self, at: u64, level: &str, message: String, fields: Vec<(String, String)>) {
        let level = match level {
            "error" => "error",
            "warn" => "warn",
            "debug" => "debug",
            "trace" => "trace",
            _ => "info",
        };
        self.push(LogLine { seq: 0, at, source: LogSource::Gateway, level, target: "gateway".into(), message, fields });
    }

    fn push(&self, mut line: LogLine) {
        if let Some(loki) = self.loki.get() {
            loki.offer(&line);
        }
        let mut inner = self.inner.lock().expect("log buffer poisoned");
        line.seq = inner.1;
        inner.1 += 1;
        if inner.0.len() == self.capacity {
            inner.0.pop_front();
        }
        inner.0.push_back(line);
    }

    /// Lines newer than `after`, at `max_rank` or more severe, containing `search`, oldest first.
    pub fn lines(&self, after: u64, max_rank: u8, search: Option<&str>, limit: usize) -> (Vec<LogLine>, u64) {
        let inner = self.inner.lock().expect("log buffer poisoned");
        let needle = search.map(str::to_lowercase).filter(|needle| !needle.is_empty());
        let matching: Vec<LogLine> = inner
            .0
            .iter()
            .filter(|line| line.seq > after && level_rank(line.level) <= max_rank)
            .filter(|line| {
                needle.as_ref().is_none_or(|needle| {
                    line.message.to_lowercase().contains(needle)
                        || line.fields.iter().any(|(_, value)| value.to_lowercase().contains(needle))
                })
            })
            .cloned()
            .collect();
        let skip = matching.len().saturating_sub(limit);
        (matching.into_iter().skip(skip).collect(), inner.1 - 1)
    }
}

pub struct LogLayer(Arc<LogBuffer>);

#[derive(Default)]
struct Fields {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.fields.push((field.name().to_owned(), value.to_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut text = String::new();
        let _ = write!(text, "{value:?}");
        if field.name() == "message" {
            self.message = text;
        } else {
            self.fields.push((field.name().to_owned(), text));
        }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default();
        self.0.push(LogLine {
            seq: 0,
            at,
            source: LogSource::Server,
            level: level_name(*event.metadata().level()),
            target: event.metadata().target().to_owned(),
            message: fields.message,
            fields: fields.fields,
        });
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn keeps_the_newest_lines_and_filters() {
        let buffer = LogBuffer::new(3);
        let subscriber = tracing_subscriber::registry().with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(login = "leni@example.org", "web login");
            tracing::warn!(ip = "192.0.2.1", "failed web login");
            tracing::info!("ready");
            tracing::error!(error = "disk full", "writing failed");
        });

        let (all, latest) = buffer.lines(0, 4, None, 100);
        assert_eq!(latest, 4);
        assert_eq!(
            all.iter().map(|l| l.message.as_str()).collect::<Vec<_>>(),
            ["failed web login", "ready", "writing failed"]
        );
        assert_eq!(all[0].fields, vec![("ip".to_owned(), "192.0.2.1".to_owned())]);

        let (warnings, _) = buffer.lines(0, level_rank("warn"), None, 100);
        assert_eq!(warnings.len(), 2);
        let (newer, _) = buffer.lines(3, 4, None, 100);
        assert_eq!(newer.len(), 1);
        let (found, _) = buffer.lines(0, 4, Some("DISK"), 100);
        assert_eq!(found[0].level, "error");
    }

    #[test]
    fn gateway_lines_sit_next_to_the_servers_own() {
        let buffer = LogBuffer::new(10);
        buffer.record_gateway(5, "warn", "did not ban an address the server asked about".into(), vec![]);
        buffer.record_gateway(6, "loud", "an unknown level".into(), vec![]);
        let (lines, _) = buffer.lines(0, 4, None, 10);
        assert_eq!((lines[0].source, lines[0].level, lines[0].at), (LogSource::Gateway, "warn", 5));
        assert_eq!(lines[1].level, "info", "an unknown level counts as info");
        assert_eq!(serde_json::to_value(&lines[0]).unwrap()["source"], "gateway");
    }
}
