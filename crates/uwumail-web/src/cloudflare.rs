//! Creating a domain's mail records at Cloudflare with an API token the admin types in once.
//! The token is only used for the request at hand and never stored or logged.

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
    /// It exists with another value; only replaced when the admin says so.
    pub wrong: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub name: String,
    pub record_type: &'static str,
    /// "created", "updated", "skipped" or "failed".
    pub outcome: &'static str,
    pub error: Option<String>,
}

/// The records a DNS check found missing or wrong, in the shape Cloudflare wants.
pub fn wanted_records(report: &DomainReport) -> Vec<WantedRecord> {
    report
        .records
        .iter()
        .filter(|record| matches!(record.status, CheckStatus::Missing | CheckStatus::Wrong))
        .filter_map(|record| {
            let wrong = record.status == CheckStatus::Wrong;
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
                        wrong,
                    })
                }
                "TXT" => Some(WantedRecord {
                    kind: record.kind,
                    record_type: "TXT",
                    name: record.name.clone(),
                    content: record.expected.clone(),
                    priority: None,
                    data: None,
                    wrong,
                }),
                "CNAME" => Some(WantedRecord {
                    kind: record.kind,
                    record_type: "CNAME",
                    name: record.name.clone(),
                    content: record.expected.clone(),
                    priority: None,
                    data: None,
                    wrong,
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
                        wrong,
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

    /// Creates what is missing; replaces wrong records only when `replace` lists their kind (e.g. mx, spf, mtasts).
    pub async fn apply(
        &self,
        domain: &str,
        wanted: &[WantedRecord],
        replace: &[String],
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
            if record.wrong && !replace.iter().any(|kind| kind == record.kind) {
                results.push(applied("skipped", None));
                continue;
            }
            let mut body =
                json!({ "type": record.record_type, "name": record.name, "content": record.content, "ttl": 1 });
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
                        let content = entry["content"].as_str().unwrap_or_default().trim_matches('"');
                        match record.kind {
                            "spf" => content.starts_with("v=spf1"),
                            "dmarc" => content.starts_with("v=DMARC1"),
                            "dkim" => content.starts_with("v=DKIM1"),
                            "tlsrpt" => content.starts_with("v=TLSRPTv1"),
                            "mtasts" => content.starts_with("v=STSv1"),
                            _ => true,
                        }
                    })
                    .collect();
                match same_kind.first().and_then(|entry| entry["id"].as_str()) {
                    Some(id) if record.wrong => {
                        self.call(Method::PUT, &format!("/zones/{zone}/dns_records/{id}"), Some(body.clone())).await?;
                        // Only one MX should remain when replacing, or mail would still go elsewhere.
                        if record.record_type == "MX" {
                            for extra in same_kind.iter().skip(1).filter_map(|entry| entry["id"].as_str()) {
                                self.call(Method::DELETE, &format!("/zones/{zone}/dns_records/{extra}"), None).await?;
                            }
                        }
                        Ok::<_, String>("updated")
                    }
                    _ => {
                        self.call(Method::POST, &format!("/zones/{zone}/dns_records"), Some(body.clone())).await?;
                        Ok("created")
                    }
                }
            }
            .await;
            results.push(match outcome {
                Ok(outcome) => applied(outcome, None),
                Err(error) => applied("failed", Some(error)),
            });
        }
        Ok(results)
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
        let result = if query.get("name").map(String::as_str) == Some("example.de") {
            json!([{ "id": "zone1", "name": "example.de" }])
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
            .filter(|r| Some(r["type"].as_str().unwrap()) == query.get("type").map(String::as_str))
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
        }
    }

    #[tokio::test]
    async fn missing_records_are_created_and_wrong_ones_only_replaced_on_request() {
        let fake = Arc::new(Fake::default());
        fake.records
            .lock()
            .unwrap()
            .push(json!({ "id": "old", "type": "TXT", "name": "example.de", "content": "\"v=spf1 -all\"" }));
        let app = Router::new()
            .route("/zones", get(zones))
            .route("/zones/{zone}/dns_records", get(list).post(create))
            .route("/zones/{zone}/dns_records/{id}", put(update))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let report = DomainReport {
            domain: "example.de".into(),
            checked_at: 0,
            source: "authoritative",
            nameservers: vec![],
            status: CheckStatus::Missing,
            records: vec![
                check("mx", "MX", "example.de", "10 mail.example.de", CheckStatus::Missing),
                check("spf", "TXT", "example.de", "v=spf1 a:mail.example.de -all", CheckStatus::Wrong),
                check("dmarc", "TXT", "_dmarc.example.de", "v=DMARC1; p=quarantine", CheckStatus::Ok),
            ],
        };
        let wanted = wanted_records(&report);
        assert_eq!(wanted.len(), 2);
        assert_eq!((wanted[0].priority, wanted[0].content.as_str()), (Some(10), "mail.example.de"));

        let cloudflare = Cloudflare::with_base("test-token", &base);
        let results = cloudflare.apply("example.de", &wanted, &[]).await.unwrap();
        assert_eq!(results.iter().map(|r| r.outcome).collect::<Vec<_>>(), ["created", "skipped"]);

        let results = cloudflare.apply("example.de", &wanted[1..], &["spf".to_owned()]).await.unwrap();
        assert_eq!(results[0].outcome, "updated");
        {
            let records = fake.records.lock().unwrap();
            assert!(records.iter().any(|r| r["content"] == "v=spf1 a:mail.example.de -all" && r["id"] == "old"));
            assert!(records.iter().any(|r| r["type"] == "MX" && r["priority"] == 10));
        }

        let error = Cloudflare::with_base("test-token", &base).zone_for("elsewhere.example").await.unwrap_err();
        assert!(error.contains("elsewhere.example"));
    }
}
