//! The gateway's own log lines, kept for the server so they can be read in its portal and go on to
//! wherever the server sends its logs. The gateway is a machine nobody logs into; this way what it
//! says ends up where people look.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use uwumail_tunnel::GatewayLogLine;

/// Lines kept while no server listens: enough for a night of quiet, and for the lines about the
/// server going away, which are the interesting ones once it is back.
pub const KEPT_LINES: usize = 1000;

pub struct LogQueue {
    capacity: usize,
    lines: Mutex<VecDeque<GatewayLogLine>>,
    arrived: Notify,
}

impl LogQueue {
    pub fn new(capacity: usize) -> Arc<LogQueue> {
        Arc::new(LogQueue { capacity, lines: Mutex::new(VecDeque::new()), arrived: Notify::new() })
    }

    /// A tracing layer that copies every event into this queue.
    pub fn layer(self: &Arc<Self>) -> LogLayer {
        LogLayer(self.clone())
    }

    /// Adds a line, dropping the oldest when full.
    pub fn push(&self, line: GatewayLogLine) {
        let mut lines = self.lines.lock().expect("log queue poisoned");
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
        drop(lines);
        self.arrived.notify_one();
    }

    /// The oldest lines, as many as fit into one tunnel message.
    pub fn take_batch(&self) -> Vec<GatewayLogLine> {
        let mut lines = self.lines.lock().expect("log queue poisoned");
        let mut batch = Vec::new();
        let mut size = 0;
        while let Some(line) = lines.front() {
            let line_size = line.size() + 1;
            if !batch.is_empty() && size + line_size > GatewayLogLine::BATCH_BYTES {
                break;
            }
            size += line_size;
            batch.push(lines.pop_front().expect("looked at it a line ago"));
        }
        batch
    }

    /// Puts a batch that could not be sent back in front, as far as there is room.
    pub fn put_back(&self, batch: Vec<GatewayLogLine>) {
        let mut lines = self.lines.lock().expect("log queue poisoned");
        for line in batch.into_iter().rev() {
            if lines.len() == self.capacity {
                break;
            }
            lines.push_front(line);
        }
    }

    /// Returns once there is at least one line.
    pub async fn wait(&self) {
        loop {
            let arrived = self.arrived.notified();
            if !self.lines.lock().expect("log queue poisoned").is_empty() {
                return;
            }
            arrived.await;
        }
    }
}

pub struct LogLayer(Arc<LogQueue>);

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

fn level_name(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default();
        self.0.push(
            GatewayLogLine {
                at,
                level: level_name(*event.metadata().level()).to_owned(),
                message: fields.message,
                fields: fields.fields,
            }
            .bounded(),
        );
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn keeps_the_newest_lines_and_hands_them_out_in_batches() {
        let queue = LogQueue::new(3);
        let subscriber = tracing_subscriber::registry().with(queue.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("one");
            tracing::info!(server = "mail.example.de", "the UwUMail server is connected");
            tracing::warn!(ip = %"192.0.2.7", "did not ban an address the server asked about");
            tracing::error!("four");
        });

        let batch = queue.take_batch();
        assert_eq!(
            batch.iter().map(|line| line.message.as_str()).collect::<Vec<_>>(),
            ["the UwUMail server is connected", "did not ban an address the server asked about", "four"]
        );
        assert_eq!(batch[0].fields, vec![("server".to_owned(), "mail.example.de".to_owned())]);
        assert_eq!(batch[1].level, "warn");
        assert!(queue.take_batch().is_empty());

        queue.put_back(batch);
        assert_eq!(queue.take_batch().len(), 3, "a batch that did not go out comes back");
    }

    #[test]
    fn a_batch_fits_into_one_tunnel_message() {
        let queue = LogQueue::new(KEPT_LINES);
        let big = "x".repeat(GatewayLogLine::MAX_TEXT);
        for _ in 0..200 {
            queue.push(GatewayLogLine { message: big.clone(), level: "info".into(), ..Default::default() });
        }
        let batch = queue.take_batch();
        assert!(batch.len() < 200);
        let json = serde_json::to_vec(&uwumail_tunnel::proto::GatewayMessage::Logs { lines: batch }).unwrap();
        assert!(json.len() <= 64 * 1024, "{} bytes", json.len());
    }
}
