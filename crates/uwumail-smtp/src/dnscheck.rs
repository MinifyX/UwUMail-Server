//! Checks the DNS records a hosted domain needs: MX, SPF, DMARC and DKIM.
//!
//! Records are resolved from the root servers down to the domain's own
//! nameservers, without the system resolver. A freshly published record shows
//! up at once, no third-party resolver learns about the domain, and local
//! split-horizon DNS cannot hide what the rest of the world sees. Where
//! outgoing DNS is blocked, the system resolver answers instead and the report
//! says so.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Instant;

use hickory_resolver::TokioResolver;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::op::Query;
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use hickory_resolver::recursor::{Recursor, RecursorOptions};
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{MessageAuthenticator, SpfResult};
use serde::Serialize;
use uwumail_store::{DkimKey, DkimKeyState};

use crate::dns::DnsCaches;

/// The IPv4 addresses of a.root-servers.net to m.root-servers.net.
const ROOT_SERVERS: [IpAddr; 13] = [
    IpAddr::V4(Ipv4Addr::new(198, 41, 0, 4)),
    IpAddr::V4(Ipv4Addr::new(170, 247, 170, 2)),
    IpAddr::V4(Ipv4Addr::new(192, 33, 4, 12)),
    IpAddr::V4(Ipv4Addr::new(199, 7, 91, 13)),
    IpAddr::V4(Ipv4Addr::new(192, 203, 230, 10)),
    IpAddr::V4(Ipv4Addr::new(192, 5, 5, 241)),
    IpAddr::V4(Ipv4Addr::new(192, 112, 36, 4)),
    IpAddr::V4(Ipv4Addr::new(198, 97, 190, 53)),
    IpAddr::V4(Ipv4Addr::new(192, 36, 148, 17)),
    IpAddr::V4(Ipv4Addr::new(192, 58, 128, 30)),
    IpAddr::V4(Ipv4Addr::new(193, 0, 14, 129)),
    IpAddr::V4(Ipv4Addr::new(199, 7, 83, 42)),
    IpAddr::V4(Ipv4Addr::new(202, 12, 27, 33)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    /// Works, but could be better (e.g. an SPF record that allows everyone).
    Warning,
    Missing,
    Wrong,
    /// The record could not be looked up.
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordCheck {
    /// "mx", "spf", "dmarc" or "dkim".
    pub kind: &'static str,
    pub name: String,
    pub record_type: &'static str,
    /// What to publish.
    pub expected: String,
    pub found: Vec<String>,
    pub status: CheckStatus,
    /// A stable reason for the app to explain, e.g. `mxUpstream` or `spfNotAllowed`.
    pub note: Option<&'static str>,
    pub selector: Option<String>,
    pub key_state: Option<DkimKeyState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainReport {
    pub domain: String,
    pub checked_at: i64,
    /// "authoritative" (resolved from the root servers) or "resolver" (the system's resolver).
    pub source: &'static str,
    pub nameservers: Vec<String>,
    /// The worst status of the records that matter now (pending DKIM keys do not count).
    pub status: CheckStatus,
    pub records: Vec<RecordCheck>,
}

/// What the server knows about how mail for a domain flows.
pub struct DomainSetup<'a> {
    pub domain: &'a str,
    pub hostname: &'a str,
    /// Outgoing mail leaves through this relay, so SPF must allow it.
    pub relay_host: Option<&'a str>,
    /// Another mail server receives first (`smtp.trusted_relays`), so MX may point there.
    pub upstream_mx: bool,
    pub dkim_keys: &'a [DkimKey],
}

pub struct DnsChecker {
    system: MessageAuthenticator,
}

fn fqdn(name: &str) -> String {
    format!("{}.", name.trim_end_matches('.'))
}

type Answer<T> = Result<Vec<T>, String>;

/// Where a check gets its answers from.
enum Lookups<'a> {
    Recursive(Box<Recursor<TokioRuntimeProvider>>),
    System(&'a TokioResolver),
}

impl Lookups<'_> {
    async fn records(&self, name: &str, record_type: RecordType) -> Answer<RData> {
        let name = Name::from_ascii(fqdn(name)).map_err(|err| err.to_string())?;
        let data: Vec<RData> = match self {
            Lookups::Recursive(recursor) => {
                match recursor.resolve(Query::query(name, record_type), Instant::now(), false).await {
                    Ok(message) => message.answers.iter().map(|record| record.data.clone()).collect(),
                    Err(err) if err.is_no_records_found() || err.is_nx_domain() => Vec::new(),
                    Err(err) => return Err(err.to_string()),
                }
            }
            Lookups::System(resolver) => match resolver.lookup(name, record_type).await {
                Ok(lookup) => lookup.answers().iter().map(|record| record.data.clone()).collect(),
                Err(err) if err.is_no_records_found() => Vec::new(),
                Err(err) => return Err(err.to_string()),
            },
        };
        // Answers can include the CNAME records that led to them.
        Ok(data.into_iter().filter(|data| data.record_type() == record_type).collect())
    }

    async fn txt(&self, name: &str) -> Answer<String> {
        Ok(self
            .records(name, RecordType::TXT)
            .await?
            .into_iter()
            .filter_map(|data| match data {
                RData::TXT(txt) => Some(txt.txt_data.iter().map(|part| String::from_utf8_lossy(part)).collect()),
                _ => None,
            })
            .collect())
    }

    async fn mx(&self, name: &str) -> Answer<(u16, String)> {
        Ok(self
            .records(name, RecordType::MX)
            .await?
            .into_iter()
            .filter_map(|data| match data {
                RData::MX(mx) => {
                    Some((mx.preference, mx.exchange.to_ascii().trim_end_matches('.').to_ascii_lowercase()))
                }
                _ => None,
            })
            .collect())
    }

    async fn addresses(&self, host: &str) -> (Vec<Ipv4Addr>, Vec<Ipv6Addr>) {
        let v4 = self.records(host, RecordType::A).await.unwrap_or_default();
        let v6 = self.records(host, RecordType::AAAA).await.unwrap_or_default();
        (
            v4.into_iter().filter_map(|data| if let RData::A(a) = data { Some(a.0) } else { None }).collect(),
            v6.into_iter().filter_map(|data| if let RData::AAAA(aaaa) = data { Some(aaaa.0) } else { None }).collect(),
        )
    }

    /// The nameservers of the zone the domain belongs to, for the report.
    async fn nameservers(&self, domain: &str) -> Vec<String> {
        let labels: Vec<&str> = domain.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let zone = labels[start..].join(".");
            let hosts: Vec<String> = self
                .records(&zone, RecordType::NS)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter_map(|data| match data {
                    RData::NS(ns) => Some(ns.0.to_ascii().trim_end_matches('.').to_ascii_lowercase()),
                    _ => None,
                })
                .collect();
            if !hosts.is_empty() {
                return hosts;
            }
        }
        Vec::new()
    }
}

impl DnsChecker {
    pub fn new() -> Result<DnsChecker, crate::SmtpError> {
        let system = MessageAuthenticator::new_system_conf()
            .or_else(|_| MessageAuthenticator::new_quad9_tls())
            .map_err(|err| crate::SmtpError::Dns(err.to_string()))?;
        Ok(DnsChecker { system })
    }

    /// Resolving from the root servers, if outgoing DNS works here; otherwise the system resolver.
    async fn lookups(&self, domain: &str) -> (Lookups<'_>, &'static str) {
        let options = RecursorOptions { response_cache_size: 0, ..RecursorOptions::default() };
        if let Ok(recursor) = Recursor::with_options(&ROOT_SERVERS, options, TokioRuntimeProvider::default()) {
            let lookups = Lookups::Recursive(Box::new(recursor));
            // An answer (even "no such record") shows the path to the authoritative servers is open.
            if lookups.records(domain, RecordType::SOA).await.is_ok() {
                return (lookups, "authoritative");
            }
        }
        (Lookups::System(self.system.resolver()), "resolver")
    }

    pub async fn check(&self, setup: DomainSetup<'_>) -> DomainReport {
        let domain = setup.domain.trim_end_matches('.').to_ascii_lowercase();
        let (lookups, source) = self.lookups(&domain).await;
        let nameservers = lookups.nameservers(&domain).await;

        let mut records = Vec::new();
        records.push(evaluate_mx(&domain, setup.hostname, setup.upstream_mx, lookups.mx(&domain).await));

        let spf_texts = lookups.txt(&domain).await;
        let mut spf = evaluate_spf_record(&domain, &setup, spf_texts);
        if spf.status == CheckStatus::Ok {
            let sender = setup.relay_host.unwrap_or(setup.hostname);
            let (v4, v6) = lookups.addresses(sender).await;
            let (status, note) = self.spf_allows(&domain, setup.hostname, sender, &spf.found[0], &v4, &v6).await;
            spf.status = status;
            spf.note = note;
        }
        records.push(spf);

        let dmarc_name = format!("_dmarc.{domain}");
        records.push(evaluate_dmarc(&domain, lookups.txt(&dmarc_name).await));

        for key in setup.dkim_keys.iter().filter(|key| key.state() != DkimKeyState::Retired) {
            let (name, _) = key.dns_record();
            records.push(evaluate_dkim(key, lookups.txt(&name).await));
        }

        let status = records
            .iter()
            .filter(|record| record.key_state != Some(DkimKeyState::Pending))
            .map(|record| record.status)
            .max()
            .unwrap_or(CheckStatus::Ok);
        DomainReport { domain, checked_at: crate::now(), source, nameservers, status, records }
    }

    /// Whether the published SPF policy lets every address of the sending host send for the domain.
    ///
    /// The policy and the sending host's addresses come from the check's own lookups; anything
    /// the policy includes from elsewhere is looked up by the system resolver.
    async fn spf_allows(
        &self,
        domain: &str,
        hostname: &str,
        sender_host: &str,
        policy: &str,
        v4: &[Ipv4Addr],
        v6: &[Ipv6Addr],
    ) -> (CheckStatus, Option<&'static str>) {
        if v4.is_empty() && v6.is_empty() {
            return (CheckStatus::Warning, Some("spfUnverified"));
        }
        let caches = DnsCaches::default();
        if caches.pin_txt(domain, policy.trim()).is_err() {
            return (CheckStatus::Wrong, Some("spfInvalid"));
        }
        caches.pin_ipv4(sender_host, v4);
        caches.pin_ipv6(sender_host, v6);
        let sender = format!("postmaster@{domain}");
        let ips = v4.iter().copied().map(IpAddr::V4).chain(v6.iter().copied().map(IpAddr::V6));
        for ip in ips {
            let output =
                self.system.verify_spf(caches.params(SpfParameters::verify(ip, hostname, hostname, &sender))).await;
            match output.result() {
                SpfResult::Pass => {}
                SpfResult::TempError => return (CheckStatus::Error, Some("lookupFailed")),
                _ => return (CheckStatus::Wrong, Some("spfNotAllowed")),
            }
        }
        (CheckStatus::Ok, None)
    }
}

fn check(kind: &'static str, name: &str, record_type: &'static str, expected: String) -> RecordCheck {
    RecordCheck {
        kind,
        name: name.to_owned(),
        record_type,
        expected,
        found: Vec::new(),
        status: CheckStatus::Ok,
        note: None,
        selector: None,
        key_state: None,
    }
}

fn failed(mut record: RecordCheck, error: &str) -> RecordCheck {
    record.status = CheckStatus::Error;
    record.note = Some("lookupFailed");
    record.found = vec![error.to_owned()];
    record
}

pub fn evaluate_mx(domain: &str, hostname: &str, upstream: bool, answer: Answer<(u16, String)>) -> RecordCheck {
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    let mut record = check("mx", domain, "MX", format!("10 {hostname}"));
    let mut found = match answer {
        Ok(found) => found,
        Err(error) => return failed(record, &error),
    };
    found.sort();
    record.found = found.iter().map(|(preference, host)| format!("{preference} {host}")).collect();
    if found.is_empty() {
        record.status = CheckStatus::Missing;
    } else if found.iter().any(|(_, host)| *host == hostname) {
        record.status = CheckStatus::Ok;
    } else if upstream {
        // Another mail server receives first and forwards to us.
        record.note = Some("mxUpstream");
    } else {
        record.status = CheckStatus::Wrong;
        record.note = Some("mxElsewhere");
    }
    record
}

/// Finds the SPF record and judges its shape; whether it allows our addresses is checked afterwards.
fn evaluate_spf_record(domain: &str, setup: &DomainSetup<'_>, answer: Answer<String>) -> RecordCheck {
    let allowed = match setup.relay_host {
        Some(relay) => format!("a:{}", relay.trim_end_matches('.')),
        None => format!("a:{}", setup.hostname.trim_end_matches('.')),
    };
    let mut record = check("spf", domain, "TXT", format!("v=spf1 {allowed} -all"));
    let texts = match answer {
        Ok(texts) => texts,
        Err(error) => return failed(record, &error),
    };
    let policies: Vec<String> =
        texts.into_iter().filter(|text| text.trim().to_ascii_lowercase().starts_with("v=spf1")).collect();
    record.found = policies.clone();
    match policies.as_slice() {
        [] => record.status = CheckStatus::Missing,
        [policy] if policy.split_whitespace().any(|term| matches!(term, "+all" | "all" | "?all")) => {
            record.status = CheckStatus::Warning;
            record.note = Some("spfTooLoose");
        }
        [_] => {}
        _ => {
            record.status = CheckStatus::Wrong;
            record.note = Some("spfMultiple");
        }
    }
    record
}

pub fn evaluate_dmarc(domain: &str, answer: Answer<String>) -> RecordCheck {
    let name = format!("_dmarc.{domain}");
    let mut record = check(
        "dmarc",
        &name,
        "TXT",
        format!("v=DMARC1; p=quarantine; adkim=s; aspf=s; rua=mailto:postmaster@{domain}"),
    );
    let texts = match answer {
        Ok(texts) => texts,
        Err(error) => return failed(record, &error),
    };
    let policies: Vec<String> =
        texts.into_iter().filter(|text| text.trim().to_ascii_uppercase().starts_with("V=DMARC1")).collect();
    record.found = policies.clone();
    match policies.as_slice() {
        [] => record.status = CheckStatus::Missing,
        [policy] => {
            let policy_value = policy
                .split(';')
                .filter_map(|tag| tag.trim().split_once('='))
                .find(|(key, _)| key.trim().eq_ignore_ascii_case("p"))
                .map(|(_, value)| value.trim().to_ascii_lowercase());
            match policy_value.as_deref() {
                Some("none") => record.note = Some("dmarcNone"),
                Some("quarantine" | "reject") => {}
                _ => {
                    record.status = CheckStatus::Wrong;
                    record.note = Some("dmarcInvalid");
                }
            }
        }
        _ => {
            record.status = CheckStatus::Wrong;
            record.note = Some("dmarcMultiple");
        }
    }
    record
}

fn dkim_public_key(text: &str) -> Option<String> {
    text.split(';')
        .filter_map(|tag| tag.trim().split_once('='))
        .find(|(key, _)| key.trim() == "p")
        .map(|(_, value)| value.chars().filter(|c| !c.is_whitespace()).collect())
}

pub fn evaluate_dkim(key: &DkimKey, answer: Answer<String>) -> RecordCheck {
    let (name, value) = key.dns_record();
    let mut record = check("dkim", &name, "TXT", value);
    record.selector = Some(key.selector.clone());
    record.key_state = Some(key.state());
    let texts = match answer {
        Ok(texts) => texts,
        Err(error) => return failed(record, &error),
    };
    record.found = texts.clone();
    let published: Vec<String> = texts.iter().filter_map(|text| dkim_public_key(text)).collect();
    if published.is_empty() {
        record.status = CheckStatus::Missing;
    } else if !published.contains(&key.public_key) {
        record.status = CheckStatus::Wrong;
        record.note = Some("dkimMismatch");
    }
    record
}

#[cfg(test)]
mod tests {
    use uwumail_store::DkimKeyAlgorithm;

    use super::*;

    fn key(active: bool) -> DkimKey {
        DkimKey {
            id: 1,
            domain: "example.de".into(),
            selector: "uwu202609r".into(),
            algorithm: DkimKeyAlgorithm::RsaSha256,
            private_key: vec![],
            public_key: "QUJDRA==".into(),
            active,
            created_at: 0,
            retired_at: None,
        }
    }

    #[test]
    fn mx_points_here_or_upstream() {
        let here = evaluate_mx("example.de", "Mail.Example.de.", false, Ok(vec![(10, "mail.example.de".into())]));
        assert_eq!((here.status, here.note), (CheckStatus::Ok, None));
        let elsewhere = evaluate_mx("example.de", "mail.example.de", false, Ok(vec![(0, "mx.other.de".into())]));
        assert_eq!((elsewhere.status, elsewhere.note), (CheckStatus::Wrong, Some("mxElsewhere")));
        let upstream = evaluate_mx("example.de", "mail.example.de", true, Ok(vec![(0, "mx.other.de".into())]));
        assert_eq!((upstream.status, upstream.note), (CheckStatus::Ok, Some("mxUpstream")));
        assert_eq!(evaluate_mx("example.de", "mail.example.de", true, Ok(vec![])).status, CheckStatus::Missing);
        assert_eq!(evaluate_mx("example.de", "m", false, Err("timeout".into())).status, CheckStatus::Error);
    }

    #[test]
    fn spf_record_shape() {
        let keys = [];
        let setup = DomainSetup {
            domain: "example.de",
            hostname: "mail.example.de",
            relay_host: Some("relay.example.net"),
            upstream_mx: false,
            dkim_keys: &keys,
        };
        let spf = |texts: &[&str]| {
            evaluate_spf_record("example.de", &setup, Ok(texts.iter().map(|t| t.to_string()).collect()))
        };
        assert_eq!(spf(&["google-site-verification=x"]).status, CheckStatus::Missing);
        assert_eq!(spf(&["v=spf1 a:relay.example.net -all"]).status, CheckStatus::Ok);
        assert_eq!(spf(&["v=spf1 a:relay.example.net -all"]).expected, "v=spf1 a:relay.example.net -all");
        assert_eq!(spf(&["v=spf1 +all"]).note, Some("spfTooLoose"));
        assert_eq!(spf(&["v=spf1 mx -all", "v=spf1 a -all"]).note, Some("spfMultiple"));
    }

    #[test]
    fn dmarc_policies() {
        let dmarc = |texts: &[&str]| evaluate_dmarc("example.de", Ok(texts.iter().map(|t| t.to_string()).collect()));
        assert_eq!(dmarc(&[]).status, CheckStatus::Missing);
        assert_eq!(dmarc(&["v=DMARC1; p=none; rua=mailto:x@example.de"]).note, Some("dmarcNone"));
        assert_eq!(dmarc(&["v=DMARC1; p=none"]).status, CheckStatus::Ok);
        assert_eq!(dmarc(&["v=DMARC1;p=reject"]).status, CheckStatus::Ok);
        assert_eq!(dmarc(&["v=DMARC1; p=maybe"]).status, CheckStatus::Wrong);
    }

    /// Asks real DNS; run with `UWUMAIL_DNSCHECK_DOMAIN=example.org cargo test -p uwumail-smtp live_check -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs the internet"]
    async fn live_check() {
        let domain = std::env::var("UWUMAIL_DNSCHECK_DOMAIN").unwrap_or_else(|_| "example.org".into());
        let hostname = std::env::var("UWUMAIL_DNSCHECK_HOSTNAME").unwrap_or_else(|_| format!("mail.{domain}"));
        let relay = std::env::var("UWUMAIL_DNSCHECK_RELAY").ok();
        let checker = DnsChecker::new().unwrap();
        let report = checker
            .check(DomainSetup {
                domain: &domain,
                hostname: &hostname,
                relay_host: relay.as_deref(),
                upstream_mx: std::env::var("UWUMAIL_DNSCHECK_UPSTREAM").is_ok(),
                dkim_keys: &[],
            })
            .await;
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        assert_eq!(report.records.len(), 3);
    }

    #[test]
    fn dkim_keys_must_match() {
        let found = |text: &str| evaluate_dkim(&key(true), Ok(vec![text.to_owned()]));
        assert_eq!(found("v=DKIM1; k=rsa; p=QUJD RA==").status, CheckStatus::Ok, "whitespace inside p= is ignored");
        assert_eq!(found("v=DKIM1; k=rsa; p=b3RoZXI=").note, Some("dkimMismatch"));
        let pending = evaluate_dkim(&key(false), Ok(vec![]));
        assert_eq!((pending.status, pending.key_state), (CheckStatus::Missing, Some(DkimKeyState::Pending)));
    }
}

/// A blocklist that marks IP addresses known for sending spam.
#[derive(Debug, Clone, Copy)]
pub struct Blocklist {
    pub name: &'static str,
    pub zone: &'static str,
    pub ipv6: bool,
}

pub const BLOCKLISTS: &[Blocklist] = &[
    Blocklist { name: "Spamhaus ZEN", zone: "zen.spamhaus.org", ipv6: true },
    Blocklist { name: "SpamCop", zone: "bl.spamcop.net", ipv6: false },
    Blocklist { name: "Barracuda", zone: "b.barracudacentral.org", ipv6: false },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListingStatus {
    Clean,
    Listed,
    /// The list did not answer usefully, e.g. because it refuses queries from this network.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub list: &'static str,
    pub status: ListingStatus,
    /// The list's answer, e.g. `127.0.0.2`.
    pub answer: Option<String>,
}

/// The name to look up for `ip` in a DNS blocklist: reversed octets (IPv4) or nibbles (IPv6).
fn blocklist_name(ip: IpAddr, zone: &str) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, d] = v4.octets();
            format!("{d}.{c}.{b}.{a}.{zone}")
        }
        IpAddr::V6(v6) => {
            let nibbles: Vec<String> =
                v6.octets().iter().flat_map(|byte| [byte >> 4, byte & 0x0f]).rev().map(|n| format!("{n:x}")).collect();
            format!("{}.{zone}", nibbles.join("."))
        }
    }
}

fn reverse_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, d] = v4.octets();
            format!("{d}.{c}.{b}.{a}.in-addr.arpa")
        }
        IpAddr::V6(v6) => {
            let nibbles: Vec<String> =
                v6.octets().iter().flat_map(|byte| [byte >> 4, byte & 0x0f]).rev().map(|n| format!("{n:x}")).collect();
            format!("{}.ip6.arpa", nibbles.join("."))
        }
    }
}

impl DnsChecker {
    /// The addresses a host name points to, as the rest of the internet sees them.
    pub async fn host_addresses(&self, host: &str) -> Vec<IpAddr> {
        let (lookups, _) = self.lookups(host).await;
        let (v4, v6) = lookups.addresses(host).await;
        v4.into_iter().map(IpAddr::V4).chain(v6.into_iter().map(IpAddr::V6)).collect()
    }

    /// The names in the PTR records of an address.
    pub async fn reverse_names(&self, ip: IpAddr) -> Result<Vec<String>, String> {
        let name = reverse_name(ip);
        let (lookups, _) = self.lookups(&name).await;
        Ok(lookups
            .records(&name, RecordType::PTR)
            .await?
            .into_iter()
            .filter_map(|data| match data {
                RData::PTR(ptr) => Some(ptr.0.to_ascii().trim_end_matches('.').to_ascii_lowercase()),
                _ => None,
            })
            .collect())
    }

    /// Whether a blocklist lists `ip`. Answers outside 127.0.0.0/8, or 127.255.255.x (Spamhaus'
    /// "you may not ask"), say nothing about the address.
    pub async fn blocklist_status(&self, ip: IpAddr, list: &Blocklist) -> Listing {
        let unknown = |answer: Option<String>| Listing { list: list.name, status: ListingStatus::Unknown, answer };
        if ip.is_ipv6() && !list.ipv6 {
            return unknown(None);
        }
        let name = blocklist_name(ip, list.zone);
        let (lookups, _) = self.lookups(list.zone).await;
        match lookups.records(&name, RecordType::A).await {
            Ok(answers) => {
                let addresses: Vec<Ipv4Addr> = answers
                    .into_iter()
                    .filter_map(|data| if let RData::A(a) = data { Some(a.0) } else { None })
                    .collect();
                match addresses.first() {
                    None => Listing { list: list.name, status: ListingStatus::Clean, answer: None },
                    Some(answer) if answer.octets()[0] == 127 && answer.octets()[1..3] != [255, 255] => {
                        Listing { list: list.name, status: ListingStatus::Listed, answer: Some(answer.to_string()) }
                    }
                    Some(answer) => unknown(Some(answer.to_string())),
                }
            }
            Err(error) => unknown(Some(error)),
        }
    }
}

#[cfg(test)]
mod lookup_name_tests {
    use super::*;

    #[test]
    fn reversed_names_for_lists_and_ptr() {
        let v4: IpAddr = "192.0.2.10".parse().unwrap();
        assert_eq!(blocklist_name(v4, "zen.spamhaus.org"), "10.2.0.192.zen.spamhaus.org");
        assert_eq!(reverse_name(v4), "10.2.0.192.in-addr.arpa");
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(reverse_name(v6).starts_with("1.0.0.0.0.0.0.0."));
        assert!(reverse_name(v6).ends_with(".8.b.d.0.1.0.0.2.ip6.arpa"));
    }
}
