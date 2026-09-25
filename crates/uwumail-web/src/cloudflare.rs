//! Creating a domain's mail records at Cloudflare with an API token the admin types in once.
//! The token is only used for the request at hand and never stored or logged.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Serialize;
use serde_json::{Value, json};
use uwumail_smtp::dnscheck::{CheckStatus, DomainReport};

pub const API: &str = "https://api.cloudflare.com/client/v4";

/// The most bytes one string inside a TXT record may hold (RFC 1035 §3.3.14).
const TXT_STRING_LIMIT: usize = 255;

/// How what is published compares with what UwUMail would publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// Nothing is published yet, so it is simply created.
    Missing,
    /// Published with a value that does not work; only replaced when the admin says so.
    Wrong,
    /// Published, working, only not written our way; only tidied when the admin says so.
    Differs,
    /// Published exactly as we would write it. At most its quoting needs a hand.
    Ours,
}

/// One record the domain needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedRecord {
    pub kind: &'static str,
    pub record_type: &'static str,
    pub name: String,
    pub content: String,
    pub priority: Option<u16>,
    /// Structured data instead of `content`, for SRV records.
    pub data: Option<Value>,
    pub state: RecordState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub name: String,
    pub record_type: &'static str,
    /// "created", "updated", "requoted", "skipped" or "failed".
    pub outcome: &'static str,
    pub error: Option<String>,
}

/// The value of a TXT record the way Cloudflare writes it: in quotes, and split into several
/// strings once it outgrows one. Cloudflare marks unquoted records in its dashboard, and a
/// quoted value longer than one string is refused.
pub fn quote_txt(value: &str) -> String {
    let mut strings: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if current.len() + character.len_utf8() > TXT_STRING_LIMIT {
            strings.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    strings.push(current);
    strings
        .iter()
        .map(|string| format!("\"{}\"", string.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a TXT record really says, whether Cloudflare holds it in quotes or bare.
pub fn unquote_txt(content: &str) -> String {
    let content = content.trim();
    if !content.starts_with('"') {
        return content.to_owned();
    }
    let mut value = String::new();
    let mut inside = false;
    let mut escaped = false;
    for character in content.chars() {
        match character {
            _ if escaped => {
                value.push(character);
                escaped = false;
            }
            '\\' if inside => escaped = true,
            '"' => inside = !inside,
            // Whitespace between two strings belongs to neither.
            _ if inside => value.push(character),
            _ => {}
        }
    }
    value
}

/// Every record UwUMail would publish, in the shape Cloudflare wants, with what the DNS check
/// saw of it. What is done with each one is up to [`Cloudflare::apply`].
pub fn wanted_records(report: &DomainReport) -> Vec<WantedRecord> {
    report
        .records
        .iter()
        // A record we could not look up stays untouched: we do not know what is there.
        .filter(|record| record.status != CheckStatus::Error)
        .filter_map(|record| {
            let state = match record.status {
                CheckStatus::Missing => RecordState::Missing,
                CheckStatus::Wrong => RecordState::Wrong,
                _ if record.differs => RecordState::Differs,
                _ => RecordState::Ours,
            };
            match record.record_type {
                "MX" => {
                    let (priority, host) = record.expected.split_once(' ')?;
                    Some(WantedRecord {
                        kind: record.kind,
                        record_type: "MX",
                        name: record.name.clone(),
                        content: host.trim_end_matches('.').to_owned(),
                        priority: priority.parse().ok(),
                        data: None,
                        state,
                    })
                }
                "TXT" => Some(WantedRecord {
                    kind: record.kind,
                    record_type: "TXT",
                    name: record.name.clone(),
                    content: record.expected.clone(),
                    priority: None,
                    data: None,
                    state,
                }),
                "CNAME" => Some(WantedRecord {
                    kind: record.kind,
                    record_type: "CNAME",
                    name: record.name.clone(),
                    content: record.expected.clone(),
                    priority: None,
                    data: None,
                    state,
                }),
                "SRV" => {
                    let parts: Vec<&str> = record.expected.split_whitespace().collect();
                    let [priority, weight, port, target] = parts.as_slice() else { return None };
                    Some(WantedRecord {
                        kind: record.kind,
                        record_type: "SRV",
                        name: record.name.clone(),
                        content: String::new(),
                        priority: None,
                        data: Some(json!({
                            "priority": priority.parse::<u16>().ok()?,
                            "weight": weight.parse::<u16>().ok()?,
                            "port": port.parse::<u16>().ok()?,
                            "target": target,
                        })),
                        state,
                    })
                }
                "CAA" => {
                    // `0 issue "<value>"`, in the shape Cloudflare takes a CAA record.
                    let value = record.expected.splitn(3, ' ').nth(2)?.trim_matches('"').to_owned();
                    Some(WantedRecord {
                        kind: record.kind,
                        record_type: "CAA",
                        name: record.name.clone(),
                        content: String::new(),
                        priority: None,
                        data: Some(json!({ "flags": 0, "tag": "issue", "value": value })),
                        // One that only warns lets others issue too, which is reason enough to
                        // replace it -- when asked, as for every CAA record.
                        state: match record.status {
                            CheckStatus::Missing => RecordState::Missing,
                            CheckStatus::Ok => RecordState::Ours,
                            _ => RecordState::Wrong,
                        },
                    })
                }
                // The MTA-STS policy file is not a DNS record.
                _ => None,
            }
        })
        .collect()
}

pub struct Cloudflare {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    base: String,
    token: String,
}

fn tls_config() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

impl Cloudflare {
    pub fn new(token: &str) -> Cloudflare {
        Cloudflare::with_base(token, API)
    }

    /// For tests against a stand-in server.
    pub fn with_base(token: &str, base: &str) -> Cloudflare {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config())
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(connector);
        Cloudflare { client, base: base.trim_end_matches('/').to_owned(), token: token.trim().to_owned() }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body.map(|b| b.to_string()).unwrap_or_default())))
            .map_err(|err| err.to_string())?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(20), self.client.request(request))
            .await
            .map_err(|_| "Cloudflare did not answer in time".to_owned())?
            .map_err(|err| format!("Cloudflare cannot be reached: {err}"))?;
        let status = response.status();
        let bytes = response.into_body().collect().await.map_err(|err| err.to_string())?.to_bytes();
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| format!("Cloudflare answered {status}"))?;
        if value["success"].as_bool() != Some(true) {
            let messages: Vec<String> = value["errors"]
                .as_array()
                .map(|errors| errors.iter().filter_map(|e| e["message"].as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            return Err(if messages.is_empty() {
                format!("Cloudflare answered {status}")
            } else {
                messages.join("; ")
            });
        }
        Ok(value)
    }

    /// The zone the domain belongs to: the domain itself or the nearest parent Cloudflare knows.
    pub async fn zone_for(&self, domain: &str) -> Result<(String, String), String> {
        let labels: Vec<&str> = domain.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let name = labels[start..].join(".");
            let found = self.call(Method::GET, &format!("/zones?name={name}"), None).await?;
            if let Some(zone) = found["result"].as_array().and_then(|zones| zones.first())
                && let Some(id) = zone["id"].as_str()
            {
                return Ok((id.to_owned(), name));
            }
        }
        Err(format!("the token cannot see a Cloudflare zone for {domain}"))
    }

    async fn existing(&self, zone: &str, record_type: &str, name: &str) -> Result<Vec<Value>, String> {
        let found =
            self.call(Method::GET, &format!("/zones/{zone}/dns_records?type={record_type}&name={name}"), None).await?;
        Ok(found["result"].as_array().cloned().unwrap_or_default())
    }

    /// Creates what is missing, replaces a broken record when `replace` lists its kind (e.g. mx,
    /// spf, mtasts), brings one that merely reads differently into our shape when `tidy` lists it,
    /// and puts the quotes around a TXT record that already says the right thing without them.
    pub async fn apply(
        &self,
        domain: &str,
        wanted: &[WantedRecord],
        replace: &[String],
        tidy: &[String],
    ) -> Result<Vec<Applied>, String> {
        let (zone, _) = self.zone_for(domain).await?;
        let mut results = Vec::new();
        for record in wanted {
            let applied = |outcome: &'static str, error: Option<String>| Applied {
                name: record.name.clone(),
                record_type: record.record_type,
                outcome,
                error,
            };
            let asked_for = match record.state {
                // Who may issue certificates for the name is only ever decided by the admin: a CAA
                // record that no longer fits the server's account stops its renewals.
                RecordState::Missing | RecordState::Wrong if record.kind == "caa" => {
                    replace.iter().any(|kind| kind == "caa")
                }
                RecordState::Wrong => replace.iter().any(|kind| kind == record.kind),
                RecordState::Differs => tidy.iter().any(|kind| kind == record.kind),
                RecordState::Missing | RecordState::Ours => true,
            };
            if !asked_for {
                results.push(applied("skipped", None));
                continue;
            }
            // Anything else that is already ours is right down to the letter; only TXT records
            // can still be missing their quotes.
            if record.state == RecordState::Ours && record.record_type != "TXT" {
                continue;
            }
            let content = if record.record_type == "TXT" { quote_txt(&record.content) } else { record.content.clone() };
            let mut body = json!({ "type": record.record_type, "name": record.name, "content": content, "ttl": 1 });
            if let Some(priority) = record.priority {
                body["priority"] = json!(priority);
            }
            if let Some(data) = &record.data {
                body["data"] = data.clone();
            }
            if record.record_type == "CNAME" {
                // Senders must reach this server itself, not a Cloudflare proxy.
                body["proxied"] = json!(false);
            }
            let outcome = async {
                let existing = self.existing(&zone, record.record_type, &record.name).await?;
                // The record of the same kind that is in the way: another MX, SPF, DMARC or DKIM value.
                let same_kind: Vec<&Value> = existing
                    .iter()
                    .filter(|entry| {
                        let content = unquote_txt(entry["content"].as_str().unwrap_or_default());
                        match record.kind {
                            "spf" => content.starts_with("v=spf1"),
                            "dmarc" => content.starts_with("v=DMARC1"),
                            "dkim" => content.starts_with("v=DKIM1"),
                            "tlsrpt" => content.starts_with("v=TLSRPTv1"),
                            "mtasts" => content.starts_with("v=STSv1"),
                            // Only who may issue; an iodef address stays.
                            "caa" => entry["data"]["tag"].as_str().is_some_and(|tag| tag.eq_ignore_ascii_case("issue")),
                            _ => true,
                        }
                    })
                    .collect();
                if record.state == RecordState::Ours {
                    // The value is right, so the only reason to write is the missing quoting.
                    let bare = same_kind.iter().find(|entry| {
                        let content = entry["content"].as_str().unwrap_or_default();
                        !content.trim_start().starts_with('"') && unquote_txt(content) == record.content
                    });
                    let Some(id) = bare.and_then(|entry| entry["id"].as_str()) else {
                        return Ok::<_, String>(None);
                    };
                    self.call(Method::PUT, &format!("/zones/{zone}/dns_records/{id}"), Some(body.clone())).await?;
                    return Ok(Some("requoted"));
                }
                match same_kind.first().and_then(|entry| entry["id"].as_str()) {
                    Some(id) => {
                        self.call(Method::PUT, &format!("/zones/{zone}/dns_records/{id}"), Some(body.clone())).await?;
                        // Only one MX should remain when replacing, or mail would still go elsewhere;
                        // only one CAA issue property, or another CA or account could still issue.
                        if matches!(record.record_type, "MX" | "CAA") && record.state != RecordState::Missing {
                            for extra in same_kind.iter().skip(1).filter_map(|entry| entry["id"].as_str()) {
                                self.call(Method::DELETE, &format!("/zones/{zone}/dns_records/{extra}"), None).await?;
                            }
                        }
                        Ok(Some("updated"))
                    }
                    None => {
                        self.call(Method::POST, &format!("/zones/{zone}/dns_records"), Some(body.clone())).await?;
                        Ok(Some("created"))
                    }
                }
            }
            .await;
            match outcome {
                Ok(Some(outcome)) => results.push(applied(outcome, None)),
                // Nothing was in the way and nothing had to change.
                Ok(None) => {}
                Err(error) => results.push(applied("failed", Some(error))),
            }
        }
        Ok(results)
    }
}

/// A host name that has to lead to the UwUMail Gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostName {
    pub name: String,
    /// The server's own name, created when it is missing. Every other name is only kept in step
    /// when it already has A or AAAA records: a CNAME follows the server's name by itself, and a
    /// name that is not there at all was never asked for.
    pub required: bool,
}

/// A record Cloudflare holds for a host name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldRecord {
    id: String,
    content: String,
    proxied: bool,
}

/// What would happen to one host name's A or AAAA records.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostChange {
    pub name: String,
    pub record_type: &'static str,
    /// The addresses Cloudflare holds now.
    pub current: Vec<String>,
    /// The gateway's addresses of this type.
    pub wanted: Vec<String>,
    /// Some of the current records go through Cloudflare's proxy, which mail cannot pass.
    pub proxied: bool,
    /// `none`, `create`, `update` (add what is missing, stop proxying), `replace` (addresses that
    /// point elsewhere go; only when confirmed) or `skip`.
    pub action: &'static str,
    /// Why a name is skipped: `noZone` (the token sees no zone for it) or `cname`.
    pub note: Option<&'static str>,
    #[serde(skip)]
    zone: String,
    #[serde(skip)]
    held: Vec<HeldRecord>,
}

impl HostChange {
    fn skipped(name: &str, note: &'static str) -> HostChange {
        HostChange {
            name: name.to_owned(),
            record_type: "A",
            current: Vec::new(),
            wanted: Vec::new(),
            proxied: false,
            action: "skip",
            note: Some(note),
            zone: String::new(),
            held: Vec::new(),
        }
    }
}

/// What to do with a host name's records of one type, given the gateway's addresses of that type.
fn host_action(held: &[HeldRecord], wanted: &[String]) -> &'static str {
    let elsewhere = held.iter().any(|record| !wanted.contains(&record.content));
    let missing = wanted.iter().any(|address| !held.iter().any(|record| &record.content == address));
    let proxied = held.iter().any(|record| record.proxied);
    if elsewhere {
        "replace"
    } else if held.is_empty() && missing {
        "create"
    } else if missing || proxied {
        "update"
    } else {
        "none"
    }
}

impl Cloudflare {
    /// The zone a name belongs to, or `None` when the token sees none; `seen` keeps the answers
    /// for the names that share a zone.
    async fn zone_of(&self, name: &str, seen: &mut HashMap<String, Option<String>>) -> Result<Option<String>, String> {
        let labels: Vec<&str> = name.split('.').collect();
        for start in 0..labels.len().saturating_sub(1) {
            let candidate = labels[start..].join(".");
            let id = match seen.get(&candidate) {
                Some(id) => id.clone(),
                None => {
                    let found = self.call(Method::GET, &format!("/zones?name={candidate}"), None).await?;
                    let id = found["result"]
                        .as_array()
                        .and_then(|zones| zones.first())
                        .and_then(|zone| zone["id"].as_str())
                        .map(str::to_owned);
                    seen.insert(candidate, id.clone());
                    id
                }
            };
            if id.is_some() {
                return Ok(id);
            }
        }
        Ok(None)
    }

    /// What pointing `hosts` at the gateway's addresses would change, without changing anything.
    pub async fn host_plan(
        &self,
        hosts: &[HostName],
        v4: &[Ipv4Addr],
        v6: &[Ipv6Addr],
    ) -> Result<Vec<HostChange>, String> {
        let v4: Vec<String> = v4.iter().map(ToString::to_string).collect();
        let v6: Vec<String> = v6.iter().map(ToString::to_string).collect();
        let mut zones = HashMap::new();
        let mut plan = Vec::new();
        for host in hosts {
            let Some(zone) = self.zone_of(&host.name, &mut zones).await? else {
                if host.required {
                    plan.push(HostChange::skipped(&host.name, "noZone"));
                }
                continue;
            };
            let found = self.call(Method::GET, &format!("/zones/{zone}/dns_records?name={}", host.name), None).await?;
            let records = found["result"].as_array().cloned().unwrap_or_default();
            let of_type = |wanted: &str| -> Vec<HeldRecord> {
                records
                    .iter()
                    .filter(|record| record["type"].as_str() == Some(wanted))
                    .map(|record| HeldRecord {
                        id: record["id"].as_str().unwrap_or_default().to_owned(),
                        content: record["content"].as_str().unwrap_or_default().to_owned(),
                        proxied: record["proxied"].as_bool().unwrap_or(false),
                    })
                    .collect()
            };
            if !of_type("CNAME").is_empty() {
                if host.required {
                    plan.push(HostChange::skipped(&host.name, "cname"));
                }
                continue;
            }
            let (a, aaaa) = (of_type("A"), of_type("AAAA"));
            if !host.required && a.is_empty() && aaaa.is_empty() {
                continue;
            }
            for (record_type, held, wanted) in [("A", a, &v4), ("AAAA", aaaa, &v6)] {
                if held.is_empty() && wanted.is_empty() {
                    continue;
                }
                plan.push(HostChange {
                    name: host.name.clone(),
                    record_type,
                    current: held.iter().map(|record| record.content.clone()).collect(),
                    proxied: held.iter().any(|record| record.proxied),
                    action: host_action(&held, wanted),
                    wanted: wanted.clone(),
                    note: None,
                    zone: zone.clone(),
                    held,
                });
            }
        }
        Ok(plan)
    }

    /// Carries out a plan from [`Cloudflare::host_plan`]. Addresses that point elsewhere are only
    /// replaced when `replace` confirms it; the records are never proxied, mail cannot pass that.
    pub async fn apply_hosts(&self, plan: &[HostChange], replace: bool) -> Vec<Applied> {
        let mut results = Vec::new();
        for change in plan {
            let applied = |outcome: &'static str, error: Option<String>| Applied {
                name: change.name.clone(),
                record_type: change.record_type,
                outcome,
                error,
            };
            match change.action {
                "none" => continue,
                "skip" => {
                    results.push(applied("skipped", None));
                    continue;
                }
                "replace" if !replace => {
                    results.push(applied("skipped", None));
                    continue;
                }
                _ => {}
            }
            let body = |content: &str| json!({ "type": change.record_type, "name": change.name, "content": content, "ttl": 1, "proxied": false });
            let outcome = async {
                let records = format!("/zones/{}/dns_records", change.zone);
                let mut missing = change
                    .wanted
                    .iter()
                    .filter(|address| !change.held.iter().any(|record| &record.content == *address));
                for record in &change.held {
                    let path = format!("{records}/{}", record.id);
                    if change.wanted.contains(&record.content) {
                        if record.proxied {
                            self.call(Method::PUT, &path, Some(body(&record.content))).await?;
                        }
                    } else if let Some(address) = missing.next() {
                        // Rewritten in place, so the name never goes without an address.
                        self.call(Method::PUT, &path, Some(body(address))).await?;
                    } else {
                        self.call(Method::DELETE, &path, None).await?;
                    }
                }
                for address in missing {
                    self.call(Method::POST, &records, Some(body(address))).await?;
                }
                Ok::<_, String>(())
            }
            .await;
            results.push(match outcome {
                Ok(()) if change.current.is_empty() => applied("created", None),
                Ok(()) => applied("updated", None),
                Err(error) => applied("failed", Some(error)),
            });
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum::Router;
    use axum::extract::{Path, Query, State};
    use axum::routing::{get, put};
    use uwumail_smtp::dnscheck::RecordCheck;

    use super::*;

    #[derive(Default)]
    struct Fake {
        records: Mutex<Vec<Value>>,
        next: Mutex<u32>,
    }

    async fn zones(Query(query): Query<std::collections::HashMap<String, String>>) -> axum::Json<Value> {
        let result = if query.get("name").map(String::as_str) == Some("example.org") {
            json!([{ "id": "zone1", "name": "example.org" }])
        } else {
            json!([])
        };
        axum::Json(json!({ "success": true, "result": result }))
    }

    async fn list(
        State(fake): State<Arc<Fake>>,
        Query(query): Query<std::collections::HashMap<String, String>>,
    ) -> axum::Json<Value> {
        let records = fake.records.lock().unwrap();
        let result: Vec<Value> = records
            .iter()
            .filter(|r| query.get("type").is_none_or(|wanted| r["type"].as_str() == Some(wanted.as_str())))
            .filter(|r| Some(r["name"].as_str().unwrap()) == query.get("name").map(String::as_str))
            .cloned()
            .collect();
        axum::Json(json!({ "success": true, "result": result }))
    }

    async fn create(State(fake): State<Arc<Fake>>, axum::Json(mut body): axum::Json<Value>) -> axum::Json<Value> {
        let mut next = fake.next.lock().unwrap();
        *next += 1;
        body["id"] = json!(format!("r{next}"));
        fake.records.lock().unwrap().push(body.clone());
        axum::Json(json!({ "success": true, "result": body }))
    }

    async fn update(
        State(fake): State<Arc<Fake>>,
        Path((_, id)): Path<(String, String)>,
        axum::Json(mut body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        body["id"] = json!(id);
        let mut records = fake.records.lock().unwrap();
        records.retain(|r| r["id"] != json!(id));
        records.push(body.clone());
        axum::Json(json!({ "success": true, "result": body }))
    }

    fn check(
        kind: &'static str,
        record_type: &'static str,
        name: &str,
        expected: &str,
        status: CheckStatus,
    ) -> RecordCheck {
        RecordCheck {
            kind,
            name: name.into(),
            record_type,
            expected: expected.into(),
            found: vec![],
            status,
            note: None,
            selector: None,
            key_state: None,
            optional: false,
            differs: false,
        }
    }

    /// A record that works, only not in our words.
    fn differing(kind: &'static str, record_type: &'static str, name: &str, expected: &str) -> RecordCheck {
        RecordCheck { differs: true, ..check(kind, record_type, name, expected, CheckStatus::Ok) }
    }

    #[test]
    fn txt_values_travel_in_quotes_and_come_back_whole() {
        assert_eq!(quote_txt("v=spf1 -all"), "\"v=spf1 -all\"");
        assert_eq!(unquote_txt("\"v=spf1 -all\""), "v=spf1 -all");
        // Cloudflare held plenty of records long before it asked for quotes.
        assert_eq!(unquote_txt("v=spf1 -all"), "v=spf1 -all");
        assert_eq!(unquote_txt("  \"v=DMARC1;\" \" p=reject\"  "), "v=DMARC1; p=reject");
        assert_eq!(unquote_txt(&quote_txt("a \"quoted\" back\\slash")), "a \"quoted\" back\\slash");

        // A DKIM key outgrows the 255 bytes one string may hold, so it goes as several.
        let key = format!("v=DKIM1; k=rsa; p={}", "A".repeat(300));
        let quoted = quote_txt(&key);
        assert_eq!(quoted, format!("\"{}\" \"{}\"", &key[..255], &key[255..]));
        assert_eq!(unquote_txt(&quoted), key);
    }

    #[tokio::test]
    async fn records_are_created_replaced_or_tidied_as_asked() {
        let key = format!("v=DKIM1; k=rsa; p={}", "A".repeat(300));
        let fake = Arc::new(Fake::default());
        fake.records.lock().unwrap().extend([
            json!({ "id": "old", "type": "TXT", "name": "example.org", "content": "\"v=spf1 -all\"" }),
            // Exactly what we would publish, only without the quotes Cloudflare now asks for.
            json!({ "id": "bare", "type": "TXT", "name": "_dmarc.example.org", "content": "v=DMARC1; p=quarantine" }),
            json!({
                "id": "tls",
                "type": "TXT",
                "name": "_smtp._tls.example.org",
                "content": "\"v=TLSRPTv1; rua=mailto:reports@other.example\"",
            }),
        ]);
        let app = Router::new()
            .route("/zones", get(zones))
            .route("/zones/{zone}/dns_records", get(list).post(create))
            .route("/zones/{zone}/dns_records/{id}", put(update))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let report = DomainReport {
            domain: "example.org".into(),
            checked_at: 0,
            source: "authoritative",
            nameservers: vec![],
            status: CheckStatus::Missing,
            records: vec![
                check("mx", "MX", "example.org", "10 mail.example.org", CheckStatus::Missing),
                check("spf", "TXT", "example.org", "v=spf1 a:mail.example.org -all", CheckStatus::Wrong),
                check("dmarc", "TXT", "_dmarc.example.org", "v=DMARC1; p=quarantine", CheckStatus::Ok),
                differing("tlsrpt", "TXT", "_smtp._tls.example.org", "v=TLSRPTv1; rua=mailto:tls@example.org"),
                check("dkim", "TXT", "uwu._domainkey.example.org", &key, CheckStatus::Missing),
            ],
        };
        let wanted = wanted_records(&report);
        assert_eq!(wanted.len(), 5);
        assert_eq!((wanted[0].priority, wanted[0].content.as_str()), (Some(10), "mail.example.org"));

        let cloudflare = Cloudflare::with_base("test-token", &base);
        let results = cloudflare.apply("example.org", &wanted, &[], &[]).await.unwrap();
        // The missing ones go in, the broken and the differing one wait for a tick, and the
        // DMARC record that was right all along only gets its quotes.
        assert_eq!(
            results.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            ["created", "skipped", "requoted", "skipped", "created"]
        );
        {
            let records = fake.records.lock().unwrap();
            let dmarc = records.iter().find(|r| r["id"] == "bare").unwrap();
            assert_eq!(dmarc["content"], "\"v=DMARC1; p=quarantine\"");
            let dkim = records.iter().find(|r| r["name"] == "uwu._domainkey.example.org").unwrap();
            assert_eq!(dkim["content"], format!("\"{}\" \"{}\"", &key[..255], &key[255..]));
            assert!(records.iter().any(|r| r["type"] == "MX" && r["priority"] == 10));
        }

        // A second run writes no duplicates, and the quoted DMARC record is left alone entirely.
        let results = cloudflare.apply("example.org", &wanted, &[], &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["updated", "skipped", "skipped", "updated"]);

        let replace = ["spf".to_owned()];
        let tidy = ["tlsrpt".to_owned()];
        let results = cloudflare.apply("example.org", &wanted[1..4], &replace, &tidy).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["updated", "updated"]);
        {
            let records = fake.records.lock().unwrap();
            assert!(records.iter().any(|r| r["content"] == "\"v=spf1 a:mail.example.org -all\"" && r["id"] == "old"));
            assert!(records.iter().any(|r| r["content"] == "\"v=TLSRPTv1; rua=mailto:tls@example.org\""));
        }

        let error = Cloudflare::with_base("test-token", &base).zone_for("elsewhere.example").await.unwrap_err();
        assert!(error.contains("elsewhere.example"));
    }

    async fn remove(State(fake): State<Arc<Fake>>, Path((_, id)): Path<(String, String)>) -> axum::Json<Value> {
        fake.records.lock().unwrap().retain(|r| r["id"] != json!(id));
        axum::Json(json!({ "success": true, "result": { "id": id } }))
    }

    #[tokio::test]
    async fn a_caa_record_is_only_ever_written_when_asked_for() {
        // security-audit-0.8.0 INF-2: it decides who may issue certificates for the host name.
        let fake = Arc::new(Fake::default());
        let app = Router::new()
            .route("/zones", get(zones))
            .route("/zones/{zone}/dns_records", get(list).post(create))
            .route("/zones/{zone}/dns_records/{id}", put(update).delete(remove))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let account = "https://acme-v02.api.letsencrypt.org/acme/acct/1";
        let value = uwumail_smtp::dnscheck::caa_value(account);
        let report = |status| DomainReport {
            domain: "example.org".into(),
            checked_at: 0,
            source: "authoritative",
            nameservers: vec![],
            status: CheckStatus::Ok,
            records: vec![check("caa", "CAA", "mail.example.org", &format!("0 issue \"{value}\""), status)],
        };
        let cloudflare = Cloudflare::with_base("test-token", &base);
        let wanted = wanted_records(&report(CheckStatus::Missing));
        let results = cloudflare.apply("example.org", &wanted, &[], &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["skipped"], "missing, but not asked for");
        assert!(fake.records.lock().unwrap().is_empty());

        let caa = ["caa".to_owned()];
        let results = cloudflare.apply("example.org", &wanted, &caa, &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["created"]);
        {
            let records = fake.records.lock().unwrap();
            assert_eq!(records[0]["type"], "CAA");
            assert_eq!(records[0]["data"], json!({ "flags": 0, "tag": "issue", "value": value }));
        }

        // One that lets others issue too is replaced, when asked, and the others go; an iodef stays.
        fake.records.lock().unwrap().extend([
            json!({ "id": "other", "type": "CAA", "name": "mail.example.org", "data": { "flags": 0, "tag": "issue", "value": "sectigo.com" } }),
            json!({ "id": "report", "type": "CAA", "name": "mail.example.org", "data": { "flags": 0, "tag": "iodef", "value": "mailto:caa@example.org" } }),
        ]);
        let wanted = wanted_records(&report(CheckStatus::Warning));
        let results = cloudflare.apply("example.org", &wanted, &[], &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["skipped"]);
        let results = cloudflare.apply("example.org", &wanted, &caa, &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["updated"]);
        let records = fake.records.lock().unwrap();
        assert_eq!(records.len(), 2, "{records:?}");
        assert!(records.iter().any(|r| r["data"]["value"] == json!(value)));
        assert!(records.iter().any(|r| r["id"] == "report"));
    }

    fn host(name: &str, required: bool) -> HostName {
        HostName { name: name.to_owned(), required }
    }

    fn summary(plan: &[HostChange]) -> Vec<(String, &'static str, &'static str)> {
        plan.iter().map(|change| (change.name.clone(), change.record_type, change.action)).collect()
    }

    #[tokio::test]
    async fn host_names_follow_the_gateway_and_foreign_addresses_wait_for_a_yes() {
        let fake = Arc::new(Fake::default());
        fake.records.lock().unwrap().extend([
            // The home connection the name pointed to before the gateway.
            json!({ "id": "home", "type": "A", "name": "mail.example.org", "content": "198.51.100.7", "proxied": false }),
            // The right address, but behind Cloudflare's proxy, which mail cannot pass.
            json!({ "id": "auto", "type": "A", "name": "autoconfig.example.org", "content": "203.0.113.5", "proxied": true }),
            // A CNAME follows the server's name by itself.
            json!({ "id": "sts", "type": "CNAME", "name": "mta-sts.example.org", "content": "mail.example.org" }),
        ]);
        let app = Router::new()
            .route("/zones", get(zones))
            .route("/zones/{zone}/dns_records", get(list).post(create))
            .route("/zones/{zone}/dns_records/{id}", put(update).delete(remove))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let cloudflare = Cloudflare::with_base("test-token", &base);
        let hosts = [
            host("mail.example.org", true),
            host("mta-sts.example.org", false),
            host("autoconfig.example.org", false),
            // Not there at all: never asked for, so left alone.
            host("imap.example.org", false),
            // In a zone the token cannot see.
            host("autoconfig.elsewhere.example", false),
        ];
        let (v4, v6) = (["203.0.113.5".parse().unwrap()], ["2001:db8::5".parse().unwrap()]);
        let plan = cloudflare.host_plan(&hosts, &v4, &v6).await.unwrap();
        let name = |name: &str| name.to_owned();
        assert_eq!(
            summary(&plan),
            [
                (name("mail.example.org"), "A", "replace"),
                (name("mail.example.org"), "AAAA", "create"),
                (name("autoconfig.example.org"), "A", "update"),
                (name("autoconfig.example.org"), "AAAA", "create"),
            ]
        );
        assert_eq!(plan[0].current, ["198.51.100.7"]);
        assert_eq!(plan[0].wanted, ["203.0.113.5"]);
        assert!(plan[2].proxied);

        // Without a yes, the address that points elsewhere stays.
        let results = cloudflare.apply_hosts(&plan, false).await;
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["skipped", "created", "updated", "created"]);
        {
            let records = fake.records.lock().unwrap();
            assert!(records.iter().any(|r| r["id"] == "home" && r["content"] == "198.51.100.7"));
            let auto = records.iter().find(|r| r["id"] == "auto").unwrap();
            assert_eq!(auto["proxied"], false);
            assert!(records.iter().all(|r| r["proxied"] != true), "{records:?}");
        }

        let plan = cloudflare.host_plan(&hosts, &v4, &v6).await.unwrap();
        assert_eq!(summary(&plan).iter().filter(|(_, _, action)| *action != "none").count(), 1);
        let results = cloudflare.apply_hosts(&plan, true).await;
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["updated"]);
        {
            let records = fake.records.lock().unwrap();
            // Rewritten in place rather than removed and added.
            assert!(records.iter().any(|r| r["id"] == "home" && r["content"] == "203.0.113.5"));
            assert!(!records.iter().any(|r| r["content"] == "198.51.100.7"));
        }
        let plan = cloudflare.host_plan(&hosts, &v4, &v6).await.unwrap();
        assert!(plan.iter().all(|change| change.action == "none"), "{:?}", summary(&plan));

        // A gateway without IPv6 means an AAAA record would lead around it: it goes, when confirmed.
        let plan = cloudflare.host_plan(&hosts[..1], &v4, &[]).await.unwrap();
        assert_eq!(
            summary(&plan),
            [(name("mail.example.org"), "A", "none"), (name("mail.example.org"), "AAAA", "replace")]
        );
        cloudflare.apply_hosts(&plan, true).await;
        assert!(!fake.records.lock().unwrap().iter().any(|r| r["type"] == "AAAA" && r["name"] == "mail.example.org"));

        // The server's own name in a zone the token cannot see is said so, not silently left out.
        let plan = cloudflare.host_plan(&[host("mail.elsewhere.example", true)], &v4, &v6).await.unwrap();
        assert_eq!((plan[0].action, plan[0].note), ("skip", Some("noZone")));
    }
}
