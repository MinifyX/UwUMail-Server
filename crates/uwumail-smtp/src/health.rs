//! What the health overview in the admin panel knows about outgoing mail: recent delivery results
//! and a probe that checks whether mail can leave at all.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use mail_auth::IpLookupStrategy;
use serde::Serialize;

use crate::client::Client;
use crate::config::{RelayConfig, RelaySecurity};
use crate::{Context, Smtp, now};

/// Delivery results are counted over this window.
const WINDOW_SECS: i64 = 24 * 3600;
const MAX_EVENTS: usize = 50_000;
const MAX_TROUBLES: usize = 500;
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// A domain with big, always reachable mail servers. The direct probe only reads their greeting.
const PROBE_DOMAIN: &str = "gmail.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryEvent {
    Delivered,
    Deferred,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Direct,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStage {
    /// The host name could not be looked up.
    Dns,
    /// No connection, or no greeting.
    Connect,
    Tls,
    Login,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub at: i64,
    pub route: Route,
    /// `host:port` that was tried.
    pub target: String,
    pub ok: bool,
    pub stage: Option<ProbeStage>,
    pub error: Option<String>,
}

/// A delivery attempt that reached no server at all, or that a relay refused to log in.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryTrouble {
    pub at: i64,
    pub domain: String,
    pub route: Route,
    pub stage: ProbeStage,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverySummary {
    pub route: Route,
    pub relay_host: Option<String>,
    /// Recipients in the last 24 hours.
    pub delivered: usize,
    pub failed: usize,
    /// Attempts that will be retried, in the last 24 hours.
    pub deferred: usize,
    pub last_delivered_at: Option<i64>,
    /// Different domains nobody could be reached for since the last successful delivery.
    pub unreachable_domains: usize,
    /// The newest trouble since the last successful delivery.
    pub last_trouble: Option<DeliveryTrouble>,
    pub probe: Option<ProbeReport>,
}

#[derive(Default)]
pub(crate) struct DeliveryStats {
    inner: Mutex<StatsInner>,
}

#[derive(Default)]
struct StatsInner {
    events: VecDeque<(i64, DeliveryEvent)>,
    last_delivered_at: Option<i64>,
    troubles: VecDeque<DeliveryTrouble>,
    probe: Option<ProbeReport>,
}

impl StatsInner {
    fn forget_old(&mut self, now: i64) {
        while self.events.front().is_some_and(|(at, _)| *at < now - WINDOW_SECS) || self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        while self.troubles.front().is_some_and(|trouble| trouble.at < now - WINDOW_SECS)
            || self.troubles.len() > MAX_TROUBLES
        {
            self.troubles.pop_front();
        }
    }
}

impl DeliveryStats {
    pub(crate) fn record(&self, event: DeliveryEvent) {
        let now = now();
        let mut inner = self.inner.lock().expect("delivery stats poisoned");
        if event == DeliveryEvent::Delivered {
            inner.last_delivered_at = Some(now);
        }
        inner.events.push_back((now, event));
        inner.forget_old(now);
    }

    pub(crate) fn trouble(&self, domain: &str, route: Route, stage: ProbeStage, error: String) {
        let now = now();
        let mut inner = self.inner.lock().expect("delivery stats poisoned");
        inner.troubles.push_back(DeliveryTrouble { at: now, domain: domain.to_owned(), route, stage, error });
        inner.forget_old(now);
    }

    fn set_probe(&self, report: ProbeReport) {
        self.inner.lock().expect("delivery stats poisoned").probe = Some(report);
    }

    fn summary(&self, route: Route, relay_host: Option<String>) -> DeliverySummary {
        let now = now();
        let mut inner = self.inner.lock().expect("delivery stats poisoned");
        inner.forget_old(now);
        let count = |kind: DeliveryEvent| inner.events.iter().filter(|(_, event)| *event == kind).count();
        let since = inner.last_delivered_at.unwrap_or(i64::MIN);
        // Only troubles on the current route count: switching to a relay starts over.
        let recent: Vec<&DeliveryTrouble> =
            inner.troubles.iter().filter(|trouble| trouble.at >= since && trouble.route == route).collect();
        DeliverySummary {
            route,
            relay_host,
            delivered: count(DeliveryEvent::Delivered),
            failed: count(DeliveryEvent::Failed),
            deferred: count(DeliveryEvent::Deferred),
            last_delivered_at: inner.last_delivered_at,
            unreachable_domains: recent.iter().map(|trouble| trouble.domain.as_str()).collect::<HashSet<_>>().len(),
            last_trouble: recent.last().map(|trouble| (*trouble).clone()),
            probe: inner.probe.clone().filter(|probe| probe.route == route),
        }
    }
}

impl Smtp {
    /// Recent delivery results for the health overview.
    pub fn delivery_summary(&self) -> DeliverySummary {
        let live = self.inner.live();
        let relay = live.delivery.relay.as_ref();
        let route = if relay.is_some() { Route::Relay } else { Route::Direct };
        self.inner.stats.summary(route, relay.map(|relay| relay.host.clone()))
    }

    /// Checks whether mail can leave: logs in to the relay, or reads the greeting of a big mail
    /// provider on port 25. Sends no mail.
    pub async fn probe_delivery(&self) -> ProbeReport {
        let ctx = &self.inner;
        let live = ctx.live();
        let report = match &live.delivery.relay {
            Some(relay) => probe_relay(ctx, relay).await,
            None => probe_direct(ctx, live.delivery.mx_port).await,
        };
        match &report.error {
            Some(error) => tracing::warn!(target = %report.target, %error, "mail cannot leave the server"),
            None => tracing::debug!(target = %report.target, "delivery probe succeeded"),
        }
        ctx.stats.set_probe(report.clone());
        report
    }
}

struct Probe {
    route: Route,
    target: String,
}

impl Probe {
    fn ok(self) -> ProbeReport {
        ProbeReport { at: now(), route: self.route, target: self.target, ok: true, stage: None, error: None }
    }

    fn failed(self, stage: ProbeStage, error: impl ToString) -> ProbeReport {
        ProbeReport {
            at: now(),
            route: self.route,
            target: self.target,
            ok: false,
            stage: Some(stage),
            error: Some(error.to_string()),
        }
    }
}

async fn probe_relay(ctx: &Context, relay: &RelayConfig) -> ProbeReport {
    let probe = Probe { route: Route::Relay, target: format!("{}:{}", relay.host, relay.port) };
    let addrs: Vec<_> = match tokio::net::lookup_host((relay.host.as_str(), relay.port)).await {
        Ok(addrs) => addrs.collect(),
        Err(err) => return probe.failed(ProbeStage::Dns, err),
    };
    let mut last_error = format!("{} has no address", relay.host);
    let mut connected = None;
    for addr in addrs {
        match Client::connect(ctx, addr, PROBE_CONNECT_TIMEOUT, PROBE_COMMAND_TIMEOUT).await {
            Ok(client) => {
                connected = Some(client);
                break;
            }
            Err(err) => last_error = err.to_string(),
        }
    }
    let Some(mut client) = connected else {
        return probe.failed(ProbeStage::Connect, last_error);
    };

    if relay.security == RelaySecurity::Tls {
        client = match client.tls_handshake(ctx.client_tls.verified.clone(), &relay.host).await {
            Ok(client) => client,
            Err(err) => return probe.failed(ProbeStage::Tls, err),
        };
    }
    match client.read_reply().await {
        Ok(greeting) if greeting.code == 220 => {}
        Ok(greeting) => return probe.failed(ProbeStage::Connect, format!("greeting: {greeting}")),
        Err(err) => return probe.failed(ProbeStage::Connect, err),
    }
    let caps = match client.ehlo(&ctx.hostname).await {
        Ok((reply, caps)) if reply.is_positive() => caps,
        Ok((reply, _)) => return probe.failed(ProbeStage::Connect, format!("EHLO: {reply}")),
        Err(err) => return probe.failed(ProbeStage::Connect, err),
    };
    if relay.security == RelaySecurity::Starttls {
        if !caps.starttls {
            return probe.failed(ProbeStage::Tls, "the relay does not offer STARTTLS");
        }
        match client.send("STARTTLS\r\n").await {
            Ok(reply) if reply.code == 220 => {}
            Ok(reply) => return probe.failed(ProbeStage::Tls, format!("STARTTLS: {reply}")),
            Err(err) => return probe.failed(ProbeStage::Tls, err),
        }
        client = match client.tls_handshake(ctx.client_tls.verified.clone(), &relay.host).await {
            Ok(client) => client,
            Err(err) => return probe.failed(ProbeStage::Tls, err),
        };
        match client.ehlo(&ctx.hostname).await {
            Ok((reply, _)) if reply.is_positive() => {}
            Ok((reply, _)) => return probe.failed(ProbeStage::Connect, format!("EHLO: {reply}")),
            Err(err) => return probe.failed(ProbeStage::Connect, err),
        }
    }
    if let (Some(username), Some(password)) = (&relay.username, &relay.password) {
        match client.auth_plain(username, password).await {
            Ok(reply) if reply.is_positive() => {}
            Ok(reply) => {
                client.quit().await;
                return probe.failed(ProbeStage::Login, reply);
            }
            Err(err) => return probe.failed(ProbeStage::Login, err),
        }
    }
    client.quit().await;
    probe.ok()
}

pub(crate) async fn probe_direct(ctx: &Context, port: u16) -> ProbeReport {
    let auth = &ctx.authenticator;
    let host = match auth.mx_lookup(PROBE_DOMAIN, Some(&ctx.dns.mx)).await {
        Ok(records) => records
            .rrset
            .iter()
            .flat_map(|mx| mx.exchanges.iter())
            .map(|host| host.trim_end_matches('.').to_owned())
            .find(|host| !host.is_empty()),
        Err(err) => {
            return Probe { route: Route::Direct, target: format!("{PROBE_DOMAIN}:{port}") }
                .failed(ProbeStage::Dns, err);
        }
    };
    let Some(host) = host else {
        return Probe { route: Route::Direct, target: format!("{PROBE_DOMAIN}:{port}") }
            .failed(ProbeStage::Dns, format!("{PROBE_DOMAIN} has no mail server"));
    };
    let probe = Probe { route: Route::Direct, target: format!("{host}:{port}") };
    let ips = match auth
        .ip_lookup(&host, IpLookupStrategy::Ipv4thenIpv6, 2, Some(&ctx.dns.ipv4), Some(&ctx.dns.ipv6))
        .await
    {
        Ok(ips) => ips,
        Err(err) => return probe.failed(ProbeStage::Dns, err),
    };
    let mut last_error = format!("{host} has no address");
    for ip in ips {
        let client = Client::connect(ctx, (ip, port).into(), PROBE_CONNECT_TIMEOUT, PROBE_COMMAND_TIMEOUT).await;
        let mut client = match client {
            Ok(client) => client,
            Err(err) => {
                last_error = err.to_string();
                continue;
            }
        };
        match client.read_reply().await {
            Ok(greeting) if greeting.code == 220 => {
                client.quit().await;
                return probe.ok();
            }
            Ok(greeting) => last_error = format!("greeting: {greeting}"),
            Err(err) => last_error = err.to_string(),
        }
    }
    probe.failed(ProbeStage::Connect, last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn troubles_count_until_the_next_delivery() {
        let stats = DeliveryStats::default();
        stats.record(DeliveryEvent::Delivered);
        stats.record(DeliveryEvent::Failed);
        std::thread::sleep(Duration::from_millis(1100));
        stats.trouble("a.example", Route::Direct, ProbeStage::Connect, "timed out".into());
        stats.trouble("b.example", Route::Direct, ProbeStage::Connect, "timed out".into());
        stats.trouble("b.example", Route::Direct, ProbeStage::Connect, "refused".into());
        stats.trouble("c.example", Route::Relay, ProbeStage::Login, "535".into());

        let summary = stats.summary(Route::Direct, None);
        assert_eq!((summary.delivered, summary.failed), (1, 1));
        assert_eq!(summary.unreachable_domains, 2, "relay troubles do not count for direct delivery");
        assert_eq!(summary.last_trouble.unwrap().error, "refused");

        std::thread::sleep(Duration::from_millis(1100));
        stats.record(DeliveryEvent::Delivered);
        let summary = stats.summary(Route::Direct, None);
        assert_eq!(summary.unreachable_domains, 0);
        assert!(summary.last_trouble.is_none());
    }
}
