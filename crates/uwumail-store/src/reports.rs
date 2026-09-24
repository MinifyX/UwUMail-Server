//! MTA-STS settings of our domains, cached policies of other domains, and the DMARC and TLS
//! reports other servers send about our domains.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::address::normalize_domain;
use crate::directory::domain_id;
use crate::{Result, Store, StoreError, now};

/// Reports are kept this long after the period they cover.
pub const REPORT_RETENTION_SECS: i64 = 180 * 24 * 3600;
/// More reports than this per domain in a day are dropped: nobody sends that many honestly.
const MAX_REPORTS_PER_DAY: i64 = 200;
/// And this many of them may come from a sender that did not pass DMARC. Anyone can address a
/// message to `dmarc-reports@`, so without a share of their own a few minutes of made-up reports
/// would use up the day's room and push the real ones out. It does not make spoofing impossible —
/// a domain of one's own passes DMARC too — but it does make it cost a domain that can be blocked.
const MAX_UNAUTHENTICATED_REPORTS_PER_DAY: i64 = 20;
const MAX_ROWS_PER_REPORT: usize = 2000;

/// The local parts of the addresses the server reads reports from.
pub const TLS_REPORT_ADDRESS: &str = "tls-reports";
pub const DMARC_REPORT_ADDRESS: &str = "dmarc-reports";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    Tls,
    Dmarc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MtaStsMode {
    Testing,
    Enforce,
}

impl MtaStsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MtaStsMode::Testing => "testing",
            MtaStsMode::Enforce => "enforce",
        }
    }

    pub fn parse(value: &str) -> Option<MtaStsMode> {
        match value {
            "testing" => Some(MtaStsMode::Testing),
            "enforce" => Some(MtaStsMode::Enforce),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MtaStsSettings {
    pub mode: MtaStsMode,
    /// The MX names the policy lists.
    pub mx: Vec<String>,
    pub changed_at: i64,
}

/// A policy another domain published, as fetched for delivering to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedStsPolicy {
    pub domain: String,
    pub policy_id: String,
    /// "enforce", "testing" or "none".
    pub mode: String,
    pub mx: Vec<String>,
    pub max_age: i64,
    pub fetched_at: i64,
}

impl CachedStsPolicy {
    pub fn expired(&self, at: i64) -> bool {
        self.fetched_at + self.max_age <= at
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsFailure {
    pub policy_type: String,
    pub result_type: String,
    pub mx_host: String,
    pub sending_ip: String,
    pub sessions: i64,
    /// Why the session failed, in the sender's own words, and where it went.
    pub failure_code: Option<String>,
    pub receiving_ip: Option<String>,
    pub helo: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTlsReport {
    pub domain: String,
    pub organization: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub authenticated: bool,
    pub successful: i64,
    pub failed: i64,
    /// The policy the sender applied, so a mismatch with ours can be seen.
    pub policy_domain: Option<String>,
    pub policy_string: Option<String>,
    pub contact: Option<String>,
    pub failures: Vec<TlsFailure>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmarcRow {
    pub source_ip: String,
    pub messages: i64,
    pub dkim_aligned: bool,
    pub spf_aligned: bool,
    pub disposition: String,
    pub header_from: String,
    /// What the reporter checked and found. `None` for rows stored before this was kept.
    pub dkim_domain: Option<String>,
    pub dkim_selector: Option<String>,
    pub dkim_result: Option<String>,
    pub spf_domain: Option<String>,
    pub spf_result: Option<String>,
    /// Why the reporter did not apply the policy it found, e.g. a forwarder it knows.
    pub override_reason: Option<String>,
    pub envelope_from: Option<String>,
    pub envelope_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewDmarcReport {
    pub domain: String,
    pub organization: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub authenticated: bool,
    /// The published policy (`none`, `quarantine`, `reject`) as the reporter saw it.
    pub policy: String,
    /// The domain the report is about, which for a subdomain is not the one we file it under.
    pub reported_domain: Option<String>,
    pub subdomain_policy: Option<String>,
    /// How strictly the reporter was told to align, e.g. `adkim=s aspf=r`.
    pub alignment: Option<String>,
    pub contact: Option<String>,
    pub rows: Vec<DmarcRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStored {
    Added,
    /// The same report arrived before.
    Duplicate,
    TooMany,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reporter {
    pub organization: String,
    pub reports: i64,
    /// Messages (DMARC) or TLS sessions.
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmarcSource {
    pub ip: String,
    pub messages: i64,
    pub passed: i64,
    pub header_from: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmarcSummary {
    pub reports: i64,
    pub unauthenticated: i64,
    pub messages: i64,
    pub passed: i64,
    pub first_begin: Option<i64>,
    pub last_end: Option<i64>,
    pub reporters: Vec<Reporter>,
    /// Sending addresses, most messages first.
    pub sources: Vec<DmarcSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsFailureSummary {
    pub result_type: String,
    pub policy_type: String,
    pub mx_host: String,
    pub sessions: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsSummary {
    pub reports: i64,
    pub unauthenticated: i64,
    pub successful: i64,
    pub failed: i64,
    pub first_begin: Option<i64>,
    pub last_end: Option<i64>,
    pub reporters: Vec<Reporter>,
    pub failures: Vec<TlsFailureSummary>,
}

/// One report in a list, whichever kind it is. `good` and `bad` are messages that passed and failed
/// for DMARC, and sessions that worked and failed for TLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportEntry {
    pub id: i64,
    pub organization: String,
    pub report_id: String,
    pub begin_at: i64,
    pub end_at: i64,
    pub received_at: i64,
    pub authenticated: bool,
    pub good: i64,
    pub bad: i64,
    /// The domain the report names, when it named one; a subdomain shows up here.
    pub about: Option<String>,
    /// DMARC only: the policy the reporter saw.
    pub policy: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportSummary {
    pub since: i64,
    pub dmarc: DmarcSummary,
    pub tls: TlsSummary,
}

fn report_domain_id(conn: &Connection, domain: &str) -> Result<i64> {
    domain_id(conn, &normalize_domain(domain)?)
}

fn too_many(conn: &Connection, table: &str, domain_id: i64, authenticated: bool) -> Result<bool> {
    let (only_theirs, limit) = match authenticated {
        true => ("", MAX_REPORTS_PER_DAY),
        false => ("AND authenticated = 0", MAX_UNAUTHENTICATED_REPORTS_PER_DAY),
    };
    let count: i64 = conn.query_row(
        &format!("SELECT count(*) FROM {table} WHERE domain_id = ?1 AND received_at > ?2 {only_theirs}"),
        params![domain_id, now() - 24 * 3600],
        |row| row.get(0),
    )?;
    Ok(count >= limit)
}

impl Store {
    /// Whether `address` is where one of our domains receives reports, and which kind.
    /// An address someone was given on purpose stays theirs.
    pub async fn report_recipient(&self, address: &str) -> Result<Option<ReportKind>> {
        let Some((local, domain)) = address.rsplit_once('@') else { return Ok(None) };
        let kind = match local.to_ascii_lowercase().as_str() {
            TLS_REPORT_ADDRESS => ReportKind::Tls,
            DMARC_REPORT_ADDRESS => ReportKind::Dmarc,
            _ => return Ok(None),
        };
        let (local, domain) = (local.to_ascii_lowercase(), domain.to_ascii_lowercase());
        self.read(move |conn| {
            let Some(domain_id): Option<i64> =
                conn.query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?
            else {
                return Ok(None);
            };
            let taken: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM addresses WHERE local_part = ?1 AND domain_id = ?2)",
                params![local, domain_id],
                |row| row.get(0),
            )?;
            Ok((!taken).then_some(kind))
        })
        .await
    }

    pub async fn mta_sts(&self, domain: &str) -> Result<Option<MtaStsSettings>> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let row: Option<(Option<String>, Option<String>, Option<i64>)> = conn
                .query_row(
                    "SELECT mta_sts_mode, mta_sts_mx, mta_sts_changed_at FROM domains WHERE name = ?1",
                    [&domain],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((Some(mode), mx, changed_at)) = row else { return Ok(None) };
            let Some(mode) = MtaStsMode::parse(&mode) else { return Ok(None) };
            let mx = mx.and_then(|mx| serde_json::from_str(&mx).ok()).unwrap_or_default();
            Ok(Some(MtaStsSettings { mode, mx, changed_at: changed_at.unwrap_or_default() }))
        })
        .await
    }

    /// Turns MTA-STS on in a mode with the MX names to list, or off with `None`.
    pub async fn set_mta_sts(&self, domain: &str, settings: Option<(MtaStsMode, Vec<String>)>) -> Result<()> {
        let domain = normalize_domain(domain)?;
        if let Some((_, mx)) = &settings
            && mx.is_empty()
        {
            return Err(StoreError::Invalid("an MTA-STS policy needs at least one MX name".into()));
        }
        self.write(move |tx| {
            let (mode, mx) = match &settings {
                Some((mode, mx)) => (Some(mode.as_str()), Some(serde_json::to_string(mx).expect("names serialize"))),
                None => (None, None),
            };
            let changed = tx.execute(
                "UPDATE domains SET mta_sts_mode = ?1, mta_sts_mx = ?2, mta_sts_changed_at = ?3 WHERE name = ?4",
                params![mode, mx, now(), domain],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("domain {domain}")));
            }
            Ok(())
        })
        .await
    }

    /// Our domains with MTA-STS on.
    pub async fn mta_sts_domains(&self) -> Result<Vec<String>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT name FROM domains WHERE mta_sts_mode IS NOT NULL ORDER BY name")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn cached_sts_policy(&self, domain: &str) -> Result<Option<CachedStsPolicy>> {
        let domain = domain.to_ascii_lowercase();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT domain, policy_id, mode, mx, max_age, fetched_at FROM mta_sts_policies WHERE domain = ?1",
                    [&domain],
                    |row| {
                        let mx: String = row.get(3)?;
                        Ok(CachedStsPolicy {
                            domain: row.get(0)?,
                            policy_id: row.get(1)?,
                            mode: row.get(2)?,
                            mx: serde_json::from_str(&mx).unwrap_or_default(),
                            max_age: row.get(4)?,
                            fetched_at: row.get(5)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn cache_sts_policy(&self, policy: CachedStsPolicy) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO mta_sts_policies (domain, policy_id, mode, mx, max_age, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    policy.domain.to_ascii_lowercase(),
                    policy.policy_id,
                    policy.mode,
                    serde_json::to_string(&policy.mx).expect("names serialize"),
                    policy.max_age,
                    policy.fetched_at
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn add_tls_report(&self, report: NewTlsReport) -> Result<ReportStored> {
        self.write(move |tx| {
            let domain_id = report_domain_id(tx, &report.domain)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM tls_reports WHERE domain_id = ?1 AND organization = ?2 AND report_id = ?3)",
                params![domain_id, report.organization, report.report_id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(ReportStored::Duplicate);
            }
            if too_many(tx, "tls_reports", domain_id, report.authenticated)? {
                return Ok(ReportStored::TooMany);
            }
            tx.execute(
                "INSERT INTO tls_reports (domain_id, organization, report_id, begin_at, end_at, received_at,
                                          authenticated, successful, failed, policy_domain, policy_string, contact)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    domain_id,
                    report.organization,
                    report.report_id,
                    report.begin_at,
                    report.end_at,
                    now(),
                    report.authenticated,
                    report.successful.max(0),
                    report.failed.max(0),
                    report.policy_domain,
                    report.policy_string,
                    report.contact
                ],
            )?;
            let id = tx.last_insert_rowid();
            let mut insert = tx.prepare(
                "INSERT INTO tls_report_failures (report_id, policy_type, result_type, mx_host, sending_ip, sessions,
                                                 failure_code, receiving_ip, helo, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for failure in report.failures.iter().take(MAX_ROWS_PER_REPORT) {
                insert.execute(params![
                    id,
                    failure.policy_type,
                    failure.result_type,
                    failure.mx_host,
                    failure.sending_ip,
                    failure.sessions.max(0),
                    failure.failure_code,
                    failure.receiving_ip,
                    failure.helo,
                    failure.detail
                ])?;
            }
            Ok(ReportStored::Added)
        })
        .await
    }

    pub async fn add_dmarc_report(&self, mut report: NewDmarcReport) -> Result<ReportStored> {
        self.write(move |tx| {
            let domain_id = report_domain_id(tx, &report.domain)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM dmarc_reports WHERE domain_id = ?1 AND organization = ?2 AND report_id = ?3)",
                params![domain_id, report.organization, report.report_id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(ReportStored::Duplicate);
            }
            if too_many(tx, "dmarc_reports", domain_id, report.authenticated)? {
                return Ok(ReportStored::TooMany);
            }
            report.rows.sort_by_key(|row| std::cmp::Reverse(row.messages));
            report.rows.truncate(MAX_ROWS_PER_REPORT);
            let messages: i64 = report.rows.iter().map(|row| row.messages.max(0)).sum();
            let passed: i64 = report
                .rows
                .iter()
                .filter(|row| row.dkim_aligned || row.spf_aligned)
                .map(|row| row.messages.max(0))
                .sum();
            tx.execute(
                "INSERT INTO dmarc_reports (domain_id, organization, report_id, begin_at, end_at, received_at,
                                            authenticated, policy, messages, passed, reported_domain,
                                            subdomain_policy, alignment, contact)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    domain_id,
                    report.organization,
                    report.report_id,
                    report.begin_at,
                    report.end_at,
                    now(),
                    report.authenticated,
                    report.policy,
                    messages,
                    passed,
                    report.reported_domain,
                    report.subdomain_policy,
                    report.alignment,
                    report.contact
                ],
            )?;
            let id = tx.last_insert_rowid();
            let mut insert = tx.prepare(
                "INSERT INTO dmarc_report_rows (report_id, source_ip, messages, dkim_aligned, spf_aligned,
                                                disposition, header_from, dkim_domain, dkim_selector, dkim_result,
                                                spf_domain, spf_result, override_reason, envelope_from, envelope_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for row in &report.rows {
                insert.execute(params![
                    id,
                    row.source_ip,
                    row.messages.max(0),
                    row.dkim_aligned,
                    row.spf_aligned,
                    row.disposition,
                    row.header_from,
                    row.dkim_domain,
                    row.dkim_selector,
                    row.dkim_result,
                    row.spf_domain,
                    row.spf_result,
                    row.override_reason,
                    row.envelope_from,
                    row.envelope_to
                ])?;
            }
            Ok(ReportStored::Added)
        })
        .await
    }

    /// The reports themselves, newest first, for reading one of them. `before` continues after the
    /// smallest id of the page before, the way the change log is paged.
    pub async fn reports(
        &self,
        domain: &str,
        kind: ReportKind,
        limit: usize,
        before: Option<i64>,
    ) -> Result<Vec<ReportEntry>> {
        let domain = normalize_domain(domain)?;
        let limit = limit.clamp(1, 200) as i64;
        let before = before.unwrap_or(i64::MAX);
        self.read(move |conn| {
            let domain_id = domain_id(conn, &domain)?;
            let sql = match kind {
                ReportKind::Dmarc => {
                    "SELECT id, organization, report_id, begin_at, end_at, received_at, authenticated, passed,
                            messages - passed, reported_domain, policy
                     FROM dmarc_reports WHERE domain_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3"
                }
                ReportKind::Tls => {
                    "SELECT id, organization, report_id, begin_at, end_at, received_at, authenticated, successful,
                            failed, policy_domain, NULL
                     FROM tls_reports WHERE domain_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3"
                }
            };
            let mut statement = conn.prepare(sql)?;
            let found = statement
                .query_map(params![domain_id, before, limit], |row| {
                    Ok(ReportEntry {
                        id: row.get(0)?,
                        organization: row.get(1)?,
                        report_id: row.get(2)?,
                        begin_at: row.get(3)?,
                        end_at: row.get(4)?,
                        received_at: row.get(5)?,
                        authenticated: row.get(6)?,
                        good: row.get(7)?,
                        bad: row.get(8)?,
                        about: row.get(9)?,
                        policy: row.get(10)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(found)
        })
        .await
    }

    /// Everything one report says. `None` when it is not this domain's.
    pub async fn dmarc_report(&self, domain: &str, id: i64) -> Result<Option<(ReportEntry, Vec<DmarcRow>)>> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let domain_id = domain_id(conn, &domain)?;
            let entry = conn
                .query_row(
                    "SELECT id, organization, report_id, begin_at, end_at, received_at, authenticated, passed,
                            messages - passed, reported_domain, policy
                     FROM dmarc_reports WHERE id = ?1 AND domain_id = ?2",
                    params![id, domain_id],
                    |row| {
                        Ok(ReportEntry {
                            id: row.get(0)?,
                            organization: row.get(1)?,
                            report_id: row.get(2)?,
                            begin_at: row.get(3)?,
                            end_at: row.get(4)?,
                            received_at: row.get(5)?,
                            authenticated: row.get(6)?,
                            good: row.get(7)?,
                            bad: row.get(8)?,
                            about: row.get(9)?,
                            policy: row.get(10)?,
                        })
                    },
                )
                .optional()?;
            let Some(entry) = entry else { return Ok(None) };
            let mut statement = conn.prepare(
                "SELECT source_ip, messages, dkim_aligned, spf_aligned, disposition, header_from, dkim_domain,
                        dkim_selector, dkim_result, spf_domain, spf_result, override_reason, envelope_from, envelope_to
                 FROM dmarc_report_rows WHERE report_id = ?1 ORDER BY messages DESC",
            )?;
            let rows = statement
                .query_map([entry.id], |row| {
                    Ok(DmarcRow {
                        source_ip: row.get(0)?,
                        messages: row.get(1)?,
                        dkim_aligned: row.get(2)?,
                        spf_aligned: row.get(3)?,
                        disposition: row.get(4)?,
                        header_from: row.get(5)?,
                        dkim_domain: row.get(6)?,
                        dkim_selector: row.get(7)?,
                        dkim_result: row.get(8)?,
                        spf_domain: row.get(9)?,
                        spf_result: row.get(10)?,
                        override_reason: row.get(11)?,
                        envelope_from: row.get(12)?,
                        envelope_to: row.get(13)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(Some((entry, rows)))
        })
        .await
    }

    /// Everything one TLS report says, with the policy the sender applied.
    pub async fn tls_report(
        &self,
        domain: &str,
        id: i64,
    ) -> Result<Option<(ReportEntry, Option<String>, Vec<TlsFailure>)>> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let domain_id = domain_id(conn, &domain)?;
            let found = conn
                .query_row(
                    "SELECT id, organization, report_id, begin_at, end_at, received_at, authenticated, successful,
                            failed, policy_domain, policy_string
                     FROM tls_reports WHERE id = ?1 AND domain_id = ?2",
                    params![id, domain_id],
                    |row| {
                        Ok((
                            ReportEntry {
                                id: row.get(0)?,
                                organization: row.get(1)?,
                                report_id: row.get(2)?,
                                begin_at: row.get(3)?,
                                end_at: row.get(4)?,
                                received_at: row.get(5)?,
                                authenticated: row.get(6)?,
                                good: row.get(7)?,
                                bad: row.get(8)?,
                                about: row.get(9)?,
                                policy: None,
                            },
                            row.get::<_, Option<String>>(10)?,
                        ))
                    },
                )
                .optional()?;
            let Some((entry, policy_string)) = found else { return Ok(None) };
            let mut statement = conn.prepare(
                "SELECT policy_type, result_type, mx_host, sending_ip, sessions, failure_code, receiving_ip, helo,
                        detail
                 FROM tls_report_failures WHERE report_id = ?1 ORDER BY sessions DESC",
            )?;
            let failures = statement
                .query_map([entry.id], |row| {
                    Ok(TlsFailure {
                        policy_type: row.get(0)?,
                        result_type: row.get(1)?,
                        mx_host: row.get(2)?,
                        sending_ip: row.get(3)?,
                        sessions: row.get(4)?,
                        failure_code: row.get(5)?,
                        receiving_ip: row.get(6)?,
                        helo: row.get(7)?,
                        detail: row.get(8)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(Some((entry, policy_string, failures)))
        })
        .await
    }

    /// What the reports for a domain say about the time since `since`.
    pub async fn report_summary(&self, domain: &str, since: i64) -> Result<ReportSummary> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            let domain_id = domain_id(conn, &domain)?;
            Ok(ReportSummary {
                since,
                dmarc: dmarc_summary(conn, domain_id, since)?,
                tls: tls_summary(conn, domain_id, since)?,
            })
        })
        .await
    }

    /// Removes reports older than `older_than_secs` and policies of other domains that expired.
    pub async fn purge_reports(&self, older_than_secs: i64) -> Result<usize> {
        self.write(move |tx| {
            let cutoff = now() - older_than_secs;
            let mut removed = tx.execute("DELETE FROM tls_reports WHERE end_at < ?1", [cutoff])?;
            removed += tx.execute("DELETE FROM dmarc_reports WHERE end_at < ?1", [cutoff])?;
            tx.execute("DELETE FROM mta_sts_policies WHERE fetched_at + max_age < ?1", [now()])?;
            Ok(removed)
        })
        .await
    }
}

fn reporters(conn: &Connection, sql: &str, domain_id: i64, since: i64) -> Result<Vec<Reporter>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![domain_id, since], |row| {
        Ok(Reporter { organization: row.get(0)?, reports: row.get(1)?, count: row.get(2)? })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn dmarc_summary(conn: &Connection, domain_id: i64, since: i64) -> Result<DmarcSummary> {
    let (reports, unauthenticated, messages, passed, first_begin, last_end) = conn.query_row(
        "SELECT count(*), coalesce(sum(authenticated = 0), 0), coalesce(sum(messages), 0), coalesce(sum(passed), 0),
                min(begin_at), max(end_at)
         FROM dmarc_reports WHERE domain_id = ?1 AND end_at >= ?2",
        params![domain_id, since],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let reporters = reporters(
        conn,
        "SELECT organization, count(*), sum(messages) FROM dmarc_reports
         WHERE domain_id = ?1 AND end_at >= ?2 GROUP BY organization ORDER BY sum(messages) DESC LIMIT 20",
        domain_id,
        since,
    )?;
    let mut sources: BTreeMap<String, DmarcSource> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT r.source_ip, r.messages, r.dkim_aligned OR r.spf_aligned, r.header_from
         FROM dmarc_report_rows r JOIN dmarc_reports d ON d.id = r.report_id
         WHERE d.domain_id = ?1 AND d.end_at >= ?2",
    )?;
    let rows = stmt.query_map(params![domain_id, since], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, bool>(2)?, row.get::<_, String>(3)?))
    })?;
    for row in rows {
        let (ip, count, pass, header_from) = row?;
        let source = sources.entry(ip.clone()).or_insert_with(|| DmarcSource {
            ip,
            messages: 0,
            passed: 0,
            header_from: Vec::new(),
        });
        source.messages += count;
        if pass {
            source.passed += count;
        }
        let header_from = header_from.to_ascii_lowercase();
        if !header_from.is_empty() && !source.header_from.contains(&header_from) && source.header_from.len() < 5 {
            source.header_from.push(header_from);
        }
    }
    let mut sources: Vec<DmarcSource> = sources.into_values().collect();
    sources.sort_by(|a, b| b.messages.cmp(&a.messages).then_with(|| a.ip.cmp(&b.ip)));
    sources.truncate(100);
    Ok(DmarcSummary { reports, unauthenticated, messages, passed, first_begin, last_end, reporters, sources })
}

fn tls_summary(conn: &Connection, domain_id: i64, since: i64) -> Result<TlsSummary> {
    let (reports, unauthenticated, successful, failed, first_begin, last_end) = conn.query_row(
        "SELECT count(*), coalesce(sum(authenticated = 0), 0), coalesce(sum(successful), 0), coalesce(sum(failed), 0),
                min(begin_at), max(end_at)
         FROM tls_reports WHERE domain_id = ?1 AND end_at >= ?2",
        params![domain_id, since],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let reporters = reporters(
        conn,
        "SELECT organization, count(*), sum(successful + failed) FROM tls_reports
         WHERE domain_id = ?1 AND end_at >= ?2 GROUP BY organization ORDER BY sum(successful + failed) DESC LIMIT 20",
        domain_id,
        since,
    )?;
    let mut stmt = conn.prepare(
        "SELECT f.result_type, f.policy_type, f.mx_host, sum(f.sessions)
         FROM tls_report_failures f JOIN tls_reports t ON t.id = f.report_id
         WHERE t.domain_id = ?1 AND t.end_at >= ?2
         GROUP BY f.result_type, f.policy_type, f.mx_host ORDER BY sum(f.sessions) DESC LIMIT 50",
    )?;
    let failures = stmt
        .query_map(params![domain_id, since], |row| {
            Ok(TlsFailureSummary {
                result_type: row.get(0)?,
                policy_type: row.get(1)?,
                mx_host: row.get(2)?,
                sessions: row.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(TlsSummary { reports, unauthenticated, successful, failed, first_begin, last_end, reporters, failures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    fn dmarc(report_id: &str, rows: Vec<DmarcRow>) -> NewDmarcReport {
        NewDmarcReport {
            domain: "example.org".into(),
            organization: "google.com".into(),
            report_id: report_id.into(),
            begin_at: now() - 86_400,
            end_at: now(),
            authenticated: true,
            policy: "quarantine".into(),
            rows,
            ..Default::default()
        }
    }

    fn row(ip: &str, messages: i64, pass: bool) -> DmarcRow {
        DmarcRow {
            source_ip: ip.into(),
            messages,
            dkim_aligned: pass,
            spf_aligned: false,
            disposition: if pass { "none" } else { "quarantine" }.into(),
            header_from: "Example.org".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn report_addresses_are_ours_unless_someone_has_them() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        assert_eq!(store.report_recipient("tls-reports@example.org").await.unwrap(), Some(ReportKind::Tls));
        assert_eq!(store.report_recipient("DMARC-Reports@example.org").await.unwrap(), Some(ReportKind::Dmarc));
        assert_eq!(store.report_recipient("dmarc-reports@elsewhere.example").await.unwrap(), None);
        assert_eq!(store.report_recipient("postmaster@example.org").await.unwrap(), None);

        store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        store.add_alias("dmarc-reports@example.org", "leni@example.org").await.unwrap();
        assert_eq!(store.report_recipient("dmarc-reports@example.org").await.unwrap(), None);
    }

    #[tokio::test]
    async fn mta_sts_settings_and_cached_policies() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        assert_eq!(store.mta_sts("example.org").await.unwrap(), None);
        assert!(store.set_mta_sts("example.org", Some((MtaStsMode::Testing, vec![]))).await.is_err());
        store.set_mta_sts("example.org", Some((MtaStsMode::Testing, vec!["mail.example.org".into()]))).await.unwrap();
        let settings = store.mta_sts("example.org").await.unwrap().unwrap();
        assert_eq!((settings.mode, settings.mx), (MtaStsMode::Testing, vec!["mail.example.org".to_owned()]));
        assert_eq!(store.mta_sts_domains().await.unwrap(), vec!["example.org".to_owned()]);
        store.set_mta_sts("example.org", None).await.unwrap();
        assert_eq!(store.mta_sts("example.org").await.unwrap(), None);

        let policy = CachedStsPolicy {
            domain: "Other.example".into(),
            policy_id: "abc".into(),
            mode: "enforce".into(),
            mx: vec!["*.other.example".into()],
            max_age: 3600,
            fetched_at: now(),
        };
        store.cache_sts_policy(policy.clone()).await.unwrap();
        let cached = store.cached_sts_policy("other.example").await.unwrap().unwrap();
        assert_eq!((cached.policy_id.as_str(), cached.mx.clone()), ("abc", policy.mx));
        assert!(!cached.expired(now()) && cached.expired(now() + 3600));
    }

    #[tokio::test]
    async fn reports_are_stored_once_and_summed_up() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let first = dmarc("r1", vec![row("192.0.2.10", 40, true), row("198.51.100.7", 3, false)]);
        assert_eq!(store.add_dmarc_report(first.clone()).await.unwrap(), ReportStored::Added);
        assert_eq!(store.add_dmarc_report(first).await.unwrap(), ReportStored::Duplicate);
        let second = dmarc("r2", vec![row("192.0.2.10", 10, true)]);
        store.add_dmarc_report(NewDmarcReport { organization: "Yahoo".into(), ..second }).await.unwrap();

        store
            .add_tls_report(NewTlsReport {
                domain: "example.org".into(),
                organization: "google.com".into(),
                report_id: "t1".into(),
                begin_at: now() - 86_400,
                end_at: now(),
                authenticated: false,
                successful: 12,
                failed: 2,
                failures: vec![TlsFailure {
                    policy_type: "sts".into(),
                    result_type: "certificate-expired".into(),
                    mx_host: "mail.example.org".into(),
                    sending_ip: "203.0.113.5".into(),
                    sessions: 2,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .await
            .unwrap();

        let summary = store.report_summary("example.org", now() - 7 * 86_400).await.unwrap();
        assert_eq!((summary.dmarc.reports, summary.dmarc.messages, summary.dmarc.passed), (2, 53, 50));
        assert_eq!(summary.dmarc.sources[0].ip, "192.0.2.10");
        assert_eq!((summary.dmarc.sources[0].messages, summary.dmarc.sources[0].passed), (50, 50));
        assert_eq!(summary.dmarc.sources[0].header_from, vec!["example.org".to_owned()]);
        assert_eq!(summary.dmarc.reporters[0].organization, "google.com");
        assert_eq!((summary.tls.successful, summary.tls.failed, summary.tls.unauthenticated), (12, 2, 1));
        assert_eq!(summary.tls.failures[0].result_type, "certificate-expired");

        assert_eq!(store.purge_reports(REPORT_RETENTION_SECS).await.unwrap(), 0);
        assert_eq!(store.purge_reports(-10).await.unwrap(), 3);
        assert_eq!(store.report_summary("example.org", 0).await.unwrap().dmarc.reports, 0);
    }
}
