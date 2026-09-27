//! Admin alerts (docs/admin-alerts.md): every few minutes the server looks at itself the way the
//! health overview does, adds what the overview cannot show (backups, certificate renewal, a new
//! version), and the store decides what is new, worse, still red or fine again. Admins get one mail
//! per look with all of it, in their own language and tone, unless they chose fewer or none.

use std::sync::Mutex;

use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use serde_json::{Value, json};
use uwumail_smtp::{InternalTone, Language};
use uwumail_store::{
    AlertEvent, AlertLevel, AlertNotice, AlertObservation, IngestRequest, MailboxRole, MailboxTarget, Role,
};

use crate::Web;
use crate::alert_texts::{self, Letter, Mood};
use crate::error::ApiResult;
use crate::health::{Health, Level, unix_now};

/// How often the server looks for alerts.
pub(crate) const ALERT_INTERVAL: i64 = 5 * 60;
/// How long after the start the first look waits, so the first DNS checks have run.
pub(crate) const ALERT_FIRST_AFTER: i64 = 2 * 60;
/// A health overview this fresh is good enough for the metrics and the alerts.
const HEALTH_FRESH_SECS: i64 = 30;
/// Backups are nightly; one that did not work for this long is worth a look.
const BACKUP_OLD_SECS: i64 = 2 * 86_400;
/// A certificate that cannot be renewed for this long is worth a mail. Single failures are common
/// (a name that does not resolve yet) and heal on their own.
const RENEWAL_FAILING_SECS: i64 = 86_400;

/// Which alert mails an admin wants, their `adminAlerts` preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailChoice {
    All,
    Problems,
    None,
}

impl MailChoice {
    pub(crate) fn from_preference(value: Option<&str>) -> MailChoice {
        match value {
            Some("problems") => MailChoice::Problems,
            Some("none") => MailChoice::None,
            _ => MailChoice::All,
        }
    }

    /// Whether this admin hears about this notice.
    pub(crate) fn wants(self, notice: &AlertNotice) -> bool {
        let level = match notice.event {
            // "Fine again" goes to whoever heard about it at its worst.
            AlertEvent::Resolved => notice.alert.notified_level.unwrap_or(notice.alert.level),
            _ => notice.alert.level,
        };
        match self {
            MailChoice::All => level >= AlertLevel::Warning,
            MailChoice::Problems => level == AlertLevel::Problem,
            MailChoice::None => false,
        }
    }
}

/// What the alerts keep between looks.
#[derive(Default)]
pub(crate) struct AlertState {
    /// The last health overview seen as nobody in particular, and when.
    health: Mutex<Option<(i64, Health)>>,
    /// Held while a look runs, so two never overlap.
    running: tokio::sync::Mutex<()>,
}

fn level_of(level: Level) -> Option<AlertLevel> {
    match level {
        Level::Warning => Some(AlertLevel::Warning),
        Level::Problem => Some(AlertLevel::Problem),
        Level::Ok | Level::Unknown => None,
    }
}

/// The alerts a health overview stands for, and the areas it could not fully check.
pub(crate) fn from_health(health: &Health) -> (Vec<AlertObservation>, Vec<String>) {
    let mut observations = Vec::new();
    let mut unsure = Vec::new();
    for area in &health.areas {
        if area.findings.iter().any(|finding| finding.level == Level::Unknown) {
            unsure.push(area.area.to_owned());
        }
        for finding in &area.findings {
            let Some(level) = level_of(finding.level) else { continue };
            let key = match finding.params.get("domain").and_then(Value::as_str) {
                Some(domain) => format!("{}:{domain}", finding.code),
                None => finding.code.to_owned(),
            };
            observations.push(AlertObservation {
                kind: area.area.to_owned(),
                key,
                code: finding.code.to_owned(),
                level,
                params: finding.params.clone(),
                link: finding.link.clone(),
            });
        }
    }
    (observations, unsure)
}

fn observation(kind: &str, code: &str, level: AlertLevel, params: Value, link: &str) -> AlertObservation {
    AlertObservation { kind: kind.into(), key: code.into(), code: code.into(), level, params, link: Some(link.into()) }
}

impl Web {
    /// The health overview as nobody in particular sees it, at most [`HEALTH_FRESH_SECS`] old.
    pub(crate) async fn server_health(&self) -> ApiResult<Health> {
        let now = unix_now();
        if let Some((at, health)) = self.inner.alerts.health.lock().expect("health cache poisoned").as_ref()
            && now - at < HEALTH_FRESH_SECS
        {
            return Ok(health.clone());
        }
        self.fresh_server_health().await
    }

    /// The health overview as nobody in particular sees it, looked at right now.
    async fn fresh_server_health(&self) -> ApiResult<Health> {
        let health = crate::health::health(self, "").await?;
        *self.inner.alerts.health.lock().expect("health cache poisoned") = Some((unix_now(), health.clone()));
        Ok(health)
    }

    /// Everything wrong besides the health overview: backups, certificate renewal, a new version.
    async fn other_observations(&self, now: i64) -> Vec<AlertObservation> {
        let mut found = Vec::new();
        if let Some(backups) = self.backups()
            && let Ok(settings) = backups.settings().await
            && settings.enabled
            && settings.target.is_some()
            && !backups.is_running()
        {
            let status = backups.status().await;
            let last = status.last_success_at.unwrap_or(0);
            let failed = status.last_error.is_some() && status.last_attempt_at.unwrap_or(0) >= last;
            if failed {
                let params = json!({ "error": status.last_error, "at": status.last_attempt_at });
                found.push(observation("backup", "backupFailed", AlertLevel::Problem, params, "/admin/backups"));
            } else if status.last_success_at.is_some_and(|at| now - at > BACKUP_OLD_SECS) {
                let params = json!({ "lastSuccessAt": status.last_success_at });
                found.push(observation("backup", "backupOld", AlertLevel::Warning, params, "/admin/backups"));
            }
        }
        match self.store().certificate_orders().await {
            Ok(orders) => {
                if let Some(since) = orders.failing_since.filter(|since| now - since > RENEWAL_FAILING_SECS) {
                    let params = json!({ "since": since, "error": orders.last_error });
                    let code = "certRenewalFailing";
                    found.push(observation("certificate", code, AlertLevel::Warning, params, "/admin/logs"));
                }
            }
            Err(err) => tracing::debug!(%err, "reading how certificate orders went failed"),
        }
        let build = crate::updates::build();
        let info = self.update_info().await;
        if build.release
            && let Some(newest) = info.releases.first()
        {
            let params = json!({ "version": newest.version });
            found.push(observation("update", "updateAvailable", AlertLevel::Info, params, "/admin/updates"));
        }
        found
    }

    /// Looks at the server once, records the alerts and writes to the admins. Public for tests;
    /// the server runs it from [`Web::run_health_checks`].
    pub async fn check_alerts(&self) {
        self.check_alerts_at(unix_now()).await;
    }

    /// [`Web::check_alerts`] as if it were `now`, so tests can let time pass.
    #[doc(hidden)]
    pub async fn check_alerts_at(&self, now: i64) {
        let _running = self.inner.alerts.running.lock().await;
        let health = match self.fresh_server_health().await {
            Ok(health) => health,
            Err(err) => {
                tracing::warn!(?err, "looking at the server's health for alerts failed");
                return;
            }
        };
        let (mut observations, unsure) = from_health(&health);
        observations.extend(self.other_observations(now).await);
        let notices = match self.store().observe_alerts(now, observations, unsure).await {
            Ok(notices) => notices,
            Err(err) => {
                tracing::warn!(%err, "recording alerts failed");
                return;
            }
        };
        for notice in &notices {
            tracing::info!(
                kind = %notice.alert.kind,
                key = %notice.alert.key,
                level = notice.alert.level.as_str(),
                event = ?notice.event,
                "admin alert"
            );
        }
        if !notices.is_empty() {
            self.mail_admins(&notices).await;
        }
    }

    /// One mail to each admin who wants to hear about any of these.
    async fn mail_admins(&self, notices: &[AlertNotice]) {
        let accounts = match self.store().accounts().await {
            Ok(accounts) => accounts,
            Err(err) => {
                tracing::warn!(%err, "listing the admins for an alert failed");
                return;
            }
        };
        for admin in accounts.iter().filter(|account| account.role == Role::Admin && account.can_log_in()) {
            let preferences = self.store().preferences(admin.id).await.unwrap_or_default();
            let choice = MailChoice::from_preference(preferences.get("adminAlerts").and_then(Value::as_str));
            let wanted: Vec<&AlertNotice> = notices.iter().filter(|notice| choice.wants(notice)).collect();
            if wanted.is_empty() {
                continue;
            }
            let language = crate::notices::language(self, &preferences);
            let tone = match preferences.get("tone").and_then(Value::as_str) {
                Some("neutral") => InternalTone::Neutral,
                Some("playful") => InternalTone::Playful,
                _ => self.smtp().tone().internal,
            };
            let tone = self.smtp().brand().internal_tone(tone);
            let raw = self.alert_mail(admin, language, tone, &wanted);
            let Some(raw) = raw else { continue };
            let Ok(Some(account_id)) = self.store().delivery_target(admin.id).await else { continue };
            let request = IngestRequest {
                account_id,
                raw,
                mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                keywords: vec![],
                received_at: None,
            };
            if let Err(err) = self.store().ingest(request).await {
                tracing::warn!(%err, login = %admin.login, "delivering an alert mail failed");
            }
        }
    }

    fn alert_mail(
        &self,
        admin: &uwumail_store::Account,
        language: Language,
        tone: InternalTone,
        notices: &[&AlertNotice],
    ) -> Option<Vec<u8>> {
        let hostname = &self.settings().hostname;
        let brand = self.smtp().brand();
        let brand = brand.name();
        let open: Vec<&&AlertNotice> = notices.iter().filter(|notice| notice.event != AlertEvent::Resolved).collect();
        let mood = if open.iter().any(|notice| notice.alert.level == AlertLevel::Problem) {
            Mood::Problem
        } else if open.is_empty() {
            Mood::Fine
        } else {
            Mood::Warning
        };
        let mut items = String::new();
        for notice in notices {
            let alert = &notice.alert;
            let domain = alert.params.get("domain").and_then(Value::as_str).unwrap_or_default();
            let text = alert_texts::finding(language, &alert.code).replace("{domain}", domain);
            let label = alert_texts::label(language, notice.event, alert.level);
            let separator = if matches!(language, Language::Ja | Language::Zh) { "：" } else { ": " };
            items.push_str(&format!("• {label}{separator}{text}\n"));
            if let Some(link) = alert.link.as_deref().filter(|_| notice.event != AlertEvent::Resolved) {
                items.push_str(&format!("  https://{hostname}{link}\n"));
            }
        }
        let name = if admin.display_name.trim().is_empty() { admin.login.as_str() } else { admin.display_name.trim() };
        let body = alert_texts::body(Letter { language, tone, mood, name, hostname, brand, items: &items });
        let subject = alert_texts::subject(language, tone, mood, hostname);
        let domain = admin.login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(hostname);
        let now = unix_now();
        MessageBuilder::new()
            .from((brand.to_owned(), format!("postmaster@{domain}")))
            .to((name.to_owned(), admin.login.clone()))
            .subject(subject)
            .date(Date::new(now))
            .message_id(format!("{}.alert@{hostname}", crate::notices::hex_id()))
            .header("Auto-Submitted", mail_builder::headers::text::Text::new("auto-generated"))
            .text_body(body)
            .write_to_vec()
            .inspect_err(|err| tracing::error!(%err, "building an alert mail failed"))
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uwumail_store::Alert;

    use super::*;
    use crate::health::{Area, Finding};

    fn notice(event: AlertEvent, level: AlertLevel, told: Option<AlertLevel>) -> AlertNotice {
        AlertNotice {
            event,
            alert: Alert {
                id: 1,
                kind: "storage".into(),
                key: "diskLow".into(),
                code: "diskLow".into(),
                level,
                params: json!({}),
                link: None,
                first_seen: 0,
                last_seen: 0,
                resolved_at: None,
                notified_at: None,
                notified_level: told,
                acknowledged_at: None,
                acknowledged_by: None,
            },
        }
    }

    #[test]
    fn admins_choose_which_mails_they_get() {
        let yellow = notice(AlertEvent::Raised, AlertLevel::Warning, Some(AlertLevel::Warning));
        let red = notice(AlertEvent::Worse, AlertLevel::Problem, Some(AlertLevel::Problem));
        let fine_after_red = notice(AlertEvent::Resolved, AlertLevel::Warning, Some(AlertLevel::Problem));
        let all = MailChoice::from_preference(None);
        let problems = MailChoice::from_preference(Some("problems"));
        let none = MailChoice::from_preference(Some("none"));
        assert!(all.wants(&yellow) && all.wants(&red) && all.wants(&fine_after_red));
        assert!(!problems.wants(&yellow) && problems.wants(&red) && problems.wants(&fine_after_red));
        assert!(!none.wants(&red));
    }

    #[test]
    fn health_findings_become_alerts_by_code_and_domain() {
        let health = Health {
            level: Level::Problem,
            checked_at: None,
            areas: vec![
                Area {
                    area: "dns",
                    level: Level::Warning,
                    findings: vec![
                        Finding {
                            code: "tlsFailures",
                            level: Level::Warning,
                            params: json!({ "domain": "example.org", "count": 2 }),
                            link: Some("/admin/reports".into()),
                        },
                        Finding {
                            code: "dnsPending",
                            level: Level::Unknown,
                            params: json!({ "count": 1 }),
                            link: None,
                        },
                    ],
                },
                Area {
                    area: "delivery",
                    level: Level::Problem,
                    findings: vec![Finding { code: "directOk", level: Level::Ok, params: Value::Null, link: None }],
                },
            ],
        };
        let (observations, unsure) = from_health(&health);
        assert_eq!(unsure, ["dns"]);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].key, "tlsFailures:example.org");
        assert_eq!(observations[0].level, AlertLevel::Warning);
    }
}
