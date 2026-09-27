//! Admin alerts (docs/admin-alerts.md): what the admins should know about, and whether they were
//! told.
//!
//! The web portal looks at the server every few minutes and hands everything it finds wrong to
//! [`Store::observe_alerts`]. That keeps one open alert per kind and key, and decides who is told
//! what: a new alert and one that got worse right away, a red one again every day until someone
//! acknowledges it, and every alert anyone was told about once more when it is fine again. An alert
//! only counts as fine when it has not been seen for [`ALERT_RESOLVE_AFTER_SECS`], so a value
//! wobbling around a limit does not write a mail each time.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, Store, StoreError, db, now};

/// How long an alert has to be gone before it counts as resolved.
pub const ALERT_RESOLVE_AFTER_SECS: i64 = 15 * 60;
/// How often the admins hear again about something red nobody acknowledged.
pub const ALERT_REMINDER_SECS: i64 = 24 * 3600;
/// How long resolved alerts stay in the history.
pub const ALERT_HISTORY_SECS: i64 = 90 * 24 * 3600;
/// Where the certificate renewal writes down how it went.
const CERTIFICATE_ORDER_KEY: &str = "acme.status";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    /// Worth knowing, nothing wrong (a new version). Never mailed.
    Info,
    Warning,
    Problem,
}

impl AlertLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertLevel::Info => "info",
            AlertLevel::Warning => "warning",
            AlertLevel::Problem => "problem",
        }
    }

    fn parse(value: &str) -> AlertLevel {
        match value {
            "problem" => AlertLevel::Problem,
            "warning" => AlertLevel::Warning,
            _ => AlertLevel::Info,
        }
    }
}

/// One thing found wrong in this round.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertObservation {
    pub kind: String,
    pub key: String,
    pub code: String,
    pub level: AlertLevel,
    pub params: Value,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: i64,
    pub kind: String,
    pub key: String,
    pub code: String,
    pub level: AlertLevel,
    pub params: Value,
    pub link: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
    pub resolved_at: Option<i64>,
    pub notified_at: Option<i64>,
    pub notified_level: Option<AlertLevel>,
    pub acknowledged_at: Option<i64>,
    pub acknowledged_by: Option<String>,
}

/// Why the admins hear about an alert now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertEvent {
    /// It is new.
    Raised,
    /// It was known, and is worse now (yellow turned red).
    Worse,
    /// Still red a day later, and nobody acknowledged it.
    Reminder,
    /// It is fine again.
    Resolved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlertNotice {
    pub event: AlertEvent,
    pub alert: Alert,
}

/// How the last certificate orders went, when one failed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CertificateOrders {
    /// The first failure since the last certificate that arrived.
    pub failing_since: Option<i64>,
    pub last_error: Option<String>,
}

const ALERT_COLUMNS: &str = "id, kind, key, code, level, params, link, first_seen, last_seen, resolved_at, \
     notified_at, notified_level, acknowledged_at, acknowledged_by";

fn alert_from_row(row: &Row<'_>) -> rusqlite::Result<Alert> {
    let params: String = row.get(5)?;
    Ok(Alert {
        id: row.get(0)?,
        kind: row.get(1)?,
        key: row.get(2)?,
        code: row.get(3)?,
        level: AlertLevel::parse(&row.get::<_, String>(4)?),
        params: serde_json::from_str(&params).unwrap_or(Value::Null),
        link: row.get(6)?,
        first_seen: row.get(7)?,
        last_seen: row.get(8)?,
        resolved_at: row.get(9)?,
        notified_at: row.get(10)?,
        notified_level: row.get::<_, Option<String>>(11)?.map(|level| AlertLevel::parse(&level)),
        acknowledged_at: row.get(12)?,
        acknowledged_by: row.get(13)?,
    })
}

/// What happens to one alert that was seen again, and whether anyone is told.
fn seen_again(alert: &mut Alert, seen: &AlertObservation, at: i64) -> Option<AlertEvent> {
    alert.code.clone_from(&seen.code);
    alert.level = seen.level;
    alert.params = seen.params.clone();
    alert.link.clone_from(&seen.link);
    alert.last_seen = at;
    if seen.level == AlertLevel::Info {
        return None;
    }
    let event = match alert.notified_level {
        None => Some(AlertEvent::Raised),
        Some(told) if seen.level > told => Some(AlertEvent::Worse),
        _ if seen.level == AlertLevel::Problem
            && alert.acknowledged_at.is_none()
            && alert.notified_at.is_none_or(|told| at - told >= ALERT_REMINDER_SECS) =>
        {
            Some(AlertEvent::Reminder)
        }
        _ => None,
    };
    if event.is_some() {
        alert.notified_at = Some(at);
        alert.notified_level = Some(alert.notified_level.map_or(seen.level, |told| told.max(seen.level)));
    }
    event
}

impl Store {
    /// Takes in what one look at the server found wrong at `at`: opens alerts for new findings,
    /// updates the known ones and resolves those gone for long enough. Kinds in `unsure` were not
    /// fully checked this time (a DNS check still running), so their alerts stay open as they are.
    /// Returns what the admins should be told.
    pub async fn observe_alerts(
        &self,
        at: i64,
        observations: Vec<AlertObservation>,
        unsure: Vec<String>,
    ) -> Result<Vec<AlertNotice>> {
        self.write(move |tx| {
            let open: Vec<Alert> = {
                let mut stmt =
                    tx.prepare(&format!("SELECT {ALERT_COLUMNS} FROM alerts WHERE resolved_at IS NULL ORDER BY id"))?;
                stmt.query_map([], alert_from_row)?.collect::<rusqlite::Result<_>>()?
            };
            let mut open: HashMap<(String, String), Alert> =
                open.into_iter().map(|alert| ((alert.kind.clone(), alert.key.clone()), alert)).collect();
            // The worst of what was seen per kind and key.
            let mut seen: HashMap<(String, String), AlertObservation> = HashMap::new();
            for observation in observations {
                let id = (observation.kind.clone(), observation.key.clone());
                match seen.get(&id) {
                    Some(known) if known.level >= observation.level => {}
                    _ => {
                        seen.insert(id, observation);
                    }
                }
            }
            let mut seen: Vec<_> = seen.into_iter().collect();
            seen.sort_by(|a, b| a.0.cmp(&b.0));

            let mut notices = Vec::new();
            for (id, observation) in seen {
                let params = observation.params.to_string();
                if let Some(mut alert) = open.remove(&id) {
                    let event = seen_again(&mut alert, &observation, at);
                    tx.execute(
                        "UPDATE alerts SET code = ?1, level = ?2, params = ?3, link = ?4, last_seen = ?5,
                             notified_at = ?6, notified_level = ?7 WHERE id = ?8",
                        params![
                            alert.code,
                            alert.level.as_str(),
                            params,
                            alert.link,
                            at,
                            alert.notified_at,
                            alert.notified_level.map(AlertLevel::as_str),
                            alert.id
                        ],
                    )?;
                    if let Some(event) = event {
                        notices.push(AlertNotice { event, alert });
                    }
                    continue;
                }
                let told = observation.level > AlertLevel::Info;
                let notified_at = told.then_some(at);
                let notified_level = told.then_some(observation.level);
                tx.execute(
                    "INSERT INTO alerts (kind, key, code, level, params, link, first_seen, last_seen, notified_at,
                         notified_level)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
                    params![
                        observation.kind,
                        observation.key,
                        observation.code,
                        observation.level.as_str(),
                        params,
                        observation.link,
                        at,
                        notified_at,
                        notified_level.map(AlertLevel::as_str)
                    ],
                )?;
                let alert = Alert {
                    id: tx.last_insert_rowid(),
                    kind: observation.kind,
                    key: observation.key,
                    code: observation.code,
                    level: observation.level,
                    params: observation.params,
                    link: observation.link,
                    first_seen: at,
                    last_seen: at,
                    resolved_at: None,
                    notified_at,
                    notified_level,
                    acknowledged_at: None,
                    acknowledged_by: None,
                };
                if told {
                    notices.push(AlertNotice { event: AlertEvent::Raised, alert });
                }
            }

            // What was not seen this time.
            let mut gone: Vec<Alert> = open.into_values().collect();
            gone.sort_by_key(|alert| alert.id);
            for mut alert in gone {
                if unsure.contains(&alert.kind) || at - alert.last_seen < ALERT_RESOLVE_AFTER_SECS {
                    continue;
                }
                tx.execute("UPDATE alerts SET resolved_at = ?1 WHERE id = ?2", params![at, alert.id])?;
                alert.resolved_at = Some(at);
                if alert.notified_level.is_some() {
                    notices.push(AlertNotice { event: AlertEvent::Resolved, alert });
                }
            }
            tx.execute("DELETE FROM alerts WHERE resolved_at < ?1", [at - ALERT_HISTORY_SECS])?;
            Ok(notices)
        })
        .await
    }

    /// The open alerts, worst and newest first, then up to `history` resolved ones, newest first.
    pub async fn alerts(&self, history: usize) -> Result<Vec<Alert>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ALERT_COLUMNS} FROM alerts WHERE resolved_at IS NULL
                 ORDER BY CASE level WHEN 'problem' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END, first_seen DESC, id DESC"
            ))?;
            let mut alerts: Vec<Alert> = stmt.query_map([], alert_from_row)?.collect::<rusqlite::Result<_>>()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {ALERT_COLUMNS} FROM alerts WHERE resolved_at IS NOT NULL
                 ORDER BY resolved_at DESC, id DESC LIMIT ?1"
            ))?;
            let resolved = stmt.query_map([history as i64], alert_from_row)?;
            for alert in resolved {
                alerts.push(alert?);
            }
            Ok(alerts)
        })
        .await
    }

    /// Open alerts per level, for the metrics.
    pub async fn open_alert_counts(&self) -> Result<Vec<(AlertLevel, i64)>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT level, COUNT(*) FROM alerts WHERE resolved_at IS NULL GROUP BY level")?;
            let rows = stmt.query_map([], |row| Ok((AlertLevel::parse(&row.get::<_, String>(0)?), row.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// An admin knows about an open alert: no more reminders for it.
    pub async fn acknowledge_alert(&self, id: i64, by: &str) -> Result<Alert> {
        let by = by.to_owned();
        let at = now();
        self.write(move |tx| {
            let changed = tx.execute(
                "UPDATE alerts SET acknowledged_at = ?1, acknowledged_by = ?2
                 WHERE id = ?3 AND resolved_at IS NULL AND acknowledged_at IS NULL",
                params![at, by, id],
            )?;
            let alert = tx
                .query_row(&format!("SELECT {ALERT_COLUMNS} FROM alerts WHERE id = ?1"), [id], alert_from_row)
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("alert {id}")))?;
            if changed == 0 && alert.resolved_at.is_some() {
                return Err(StoreError::Rule { code: "alertResolved", message: "this alert is resolved".into() });
            }
            Ok(alert)
        })
        .await
    }

    /// Writes down how ordering a certificate went: `None` for a certificate that arrived.
    pub async fn note_certificate_order(&self, error: Option<String>) -> Result<()> {
        let at = now();
        self.write(move |tx| {
            let Some(error) = error else {
                db::delete_setting(tx, CERTIFICATE_ORDER_KEY)?;
                return Ok(());
            };
            let mut orders: CertificateOrders = db::get_setting(tx, CERTIFICATE_ORDER_KEY)?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            orders.failing_since.get_or_insert(at);
            orders.last_error = Some(error.chars().take(500).collect());
            db::set_setting(tx, CERTIFICATE_ORDER_KEY, &serde_json::to_string(&orders).expect("serializes"))?;
            Ok(())
        })
        .await
    }

    pub async fn certificate_orders(&self) -> Result<CertificateOrders> {
        Ok(self
            .setting(CERTIFICATE_ORDER_KEY)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::store;

    const T0: i64 = 1_790_510_400;
    const MINUTE: i64 = 60;
    const HOUR: i64 = 3600;

    fn seen(kind: &str, key: &str, level: AlertLevel) -> AlertObservation {
        AlertObservation {
            kind: kind.into(),
            key: key.into(),
            code: key.split(':').next().unwrap().into(),
            level,
            params: json!({ "domain": "example.org" }),
            link: Some("/admin/domains".into()),
        }
    }

    fn events(notices: &[AlertNotice]) -> Vec<(AlertEvent, &str)> {
        notices.iter().map(|notice| (notice.event, notice.alert.key.as_str())).collect()
    }

    #[tokio::test]
    async fn alerts_are_raised_worsened_reminded_and_resolved() {
        let (store, _dir) = store().await;
        let disk = || seen("storage", "diskLow", AlertLevel::Warning);

        let notices = store.observe_alerts(T0, vec![disk()], vec![]).await.unwrap();
        assert_eq!(events(&notices), [(AlertEvent::Raised, "diskLow")]);
        // Seen again and again: one mail was enough.
        for step in 1..10 {
            let notices = store.observe_alerts(T0 + step * 5 * MINUTE, vec![disk()], vec![]).await.unwrap();
            assert!(notices.is_empty());
        }
        // Red now: that is worth another mail, and then one a day while it stays red.
        let red = seen("storage", "diskLow", AlertLevel::Problem);
        let at = T0 + HOUR;
        assert_eq!(
            events(&store.observe_alerts(at, vec![red.clone()], vec![]).await.unwrap()),
            [(AlertEvent::Worse, "diskLow")]
        );
        assert!(store.observe_alerts(at + 23 * HOUR, vec![red.clone()], vec![]).await.unwrap().is_empty());
        assert_eq!(
            events(&store.observe_alerts(at + 24 * HOUR, vec![red.clone()], vec![]).await.unwrap()),
            [(AlertEvent::Reminder, "diskLow")]
        );
        // Back to yellow: nobody is told, it was worse before.
        assert!(store.observe_alerts(at + 25 * HOUR, vec![disk()], vec![]).await.unwrap().is_empty());

        // Gone for a moment only is not gone.
        let later = at + 26 * HOUR;
        assert!(store.observe_alerts(later, vec![disk()], vec![]).await.unwrap().is_empty());
        assert!(store.observe_alerts(later + 5 * MINUTE, vec![], vec![]).await.unwrap().is_empty());
        let notices = store.observe_alerts(later + 30 * MINUTE, vec![], vec!["storage".into()]).await.unwrap();
        assert!(notices.is_empty(), "a kind that was not fully checked keeps its alerts");
        let notices = store.observe_alerts(later + 30 * MINUTE, vec![], vec![]).await.unwrap();
        assert_eq!(events(&notices), [(AlertEvent::Resolved, "diskLow")]);
        assert_eq!(notices[0].alert.resolved_at, Some(later + 30 * MINUTE));

        let alerts = store.alerts(10).await.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].notified_level, Some(AlertLevel::Problem));
        // Coming back later is a new alert.
        let again = store.observe_alerts(later + HOUR, vec![disk()], vec![]).await.unwrap();
        assert_eq!(events(&again), [(AlertEvent::Raised, "diskLow")]);
        assert_ne!(again[0].alert.id, alerts[0].id);
    }

    #[tokio::test]
    async fn acknowledged_alerts_are_not_reminded_and_information_is_never_mailed() {
        let (store, _dir) = store().await;
        let red = seen("delivery", "queueStuck", AlertLevel::Problem);
        let update = seen("update", "update", AlertLevel::Info);
        let notices = store.observe_alerts(T0, vec![red.clone(), update.clone()], vec![]).await.unwrap();
        assert_eq!(events(&notices), [(AlertEvent::Raised, "queueStuck")]);
        let id = notices[0].alert.id;

        let acknowledged = store.acknowledge_alert(id, "nyu@example.org").await.unwrap();
        assert_eq!(acknowledged.acknowledged_by.as_deref(), Some("nyu@example.org"));
        let notices = store.observe_alerts(T0 + 48 * HOUR, vec![red.clone(), update.clone()], vec![]).await.unwrap();
        assert!(notices.is_empty(), "no reminders once acknowledged");

        // The open ones come first, the worst of them on top.
        let alerts = store.alerts(10).await.unwrap();
        assert_eq!(alerts.iter().map(|alert| alert.key.as_str()).collect::<Vec<_>>(), ["queueStuck", "update"]);
        assert_eq!(store.open_alert_counts().await.unwrap().len(), 2);

        // Information comes and goes without a mail.
        let notices = store.observe_alerts(T0 + 49 * HOUR, vec![red], vec![]).await.unwrap();
        assert!(notices.is_empty());
        let alerts = store.alerts(10).await.unwrap();
        assert!(alerts.iter().any(|alert| alert.key == "update" && alert.resolved_at.is_some()));
        let resolved = alerts.iter().find(|alert| alert.key == "update").unwrap();
        assert!(matches!(
            store.acknowledge_alert(resolved.id, "nyu@example.org").await,
            Err(StoreError::Rule { code: "alertResolved", .. })
        ));
        assert!(matches!(store.acknowledge_alert(9999, "nyu@example.org").await, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn certificate_orders_remember_when_they_started_failing() {
        let (store, _dir) = store().await;
        assert_eq!(store.certificate_orders().await.unwrap(), CertificateOrders::default());
        store.note_certificate_order(Some("timeout".into())).await.unwrap();
        let first = store.certificate_orders().await.unwrap();
        store.note_certificate_order(Some("refused".into())).await.unwrap();
        let second = store.certificate_orders().await.unwrap();
        assert_eq!(second.failing_since, first.failing_since);
        assert_eq!(second.last_error.as_deref(), Some("refused"));
        store.note_certificate_order(None).await.unwrap();
        assert_eq!(store.certificate_orders().await.unwrap(), CertificateOrders::default());
    }
}
