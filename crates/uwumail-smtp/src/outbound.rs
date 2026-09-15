//! The delivery worker: takes due messages from the queue and hands them to other servers.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use mail_auth::{DnsError, IpLookupStrategy};
use smtp_proto::RCPT_NOTIFY_NEVER;
use tokio::sync::watch;
use uwumail_store::{QueueRecipient, QueuedMessage};

use crate::client::{Client, Reply};
use crate::config::{RelayConfig, RelaySecurity};
use crate::dsn::{self, FailedRecipient};
use crate::health::{DeliveryEvent, ProbeStage, Route};
use crate::mta_sts;
use crate::{Context, Smtp, now};

/// How long claimed recipients stay reserved for one delivery attempt.
const LEASE_SECS: i64 = 3600;
const MAX_HOSTS: usize = 5;
const MAX_ADDRESSES_PER_HOST: usize = 3;

/// Runs until `shutdown` changes.
pub async fn run_queue(smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    loop {
        match ctx.store.claim_due_deliveries(200, LEASE_SECS).await {
            Ok(entries) => {
                for entry in entries {
                    let recipients: Vec<QueueRecipient> = {
                        let mut inflight = ctx.inflight.lock().expect("inflight set poisoned");
                        entry.recipients.into_iter().filter(|r| inflight.insert(r.id)).collect()
                    };
                    let mut by_domain: BTreeMap<String, Vec<QueueRecipient>> = BTreeMap::new();
                    for recipient in recipients {
                        by_domain.entry(recipient.domain.clone()).or_default().push(recipient);
                    }
                    for (domain, group) in by_domain {
                        let smtp = smtp.clone();
                        let message = entry.message.clone();
                        tokio::spawn(async move {
                            let Ok(_permit) = smtp.inner.delivery_permits.clone().acquire_owned().await else {
                                return;
                            };
                            deliver_group(&smtp.inner, message, domain, group).await;
                        });
                    }
                }
            }
            Err(err) => tracing::error!(%err, "reading the delivery queue failed"),
        }

        let wait = match ctx.store.next_queue_attempt_at().await {
            Ok(Some(at)) => (at - now()).clamp(1, 60),
            _ => 60,
        };
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(wait as u64)) => {}
            _ = ctx.store.queue_wakeup() => {}
            _ = shutdown.changed() => break,
        }
    }
}

#[derive(Debug, Clone)]
enum Outcome {
    Delivered(String),
    Deferred(String),
    Failed(String),
}

impl Outcome {
    fn from_reply(reply: &Reply) -> Outcome {
        if reply.is_positive() {
            Outcome::Delivered(reply.to_string())
        } else if reply.is_permanent() {
            Outcome::Failed(reply.to_string())
        } else {
            Outcome::Deferred(reply.to_string())
        }
    }
}

/// Waiting time before the next attempt, growing with each failure.
fn retry_delay(attempt: i64) -> i64 {
    const STEPS: [i64; 7] = [300, 900, 1800, 3600, 7200, 10_800, 21_600];
    STEPS[(attempt.max(1) as usize - 1).min(STEPS.len() - 1)]
}

async fn deliver_group(ctx: &Context, message: QueuedMessage, domain: String, recipients: Vec<QueueRecipient>) {
    let outcomes = match ctx.store.blob(&message.blob).await {
        Ok(raw) => {
            let outcomes = deliver_domain(ctx, &domain, &message, &raw, &recipients).await;
            Some((raw, outcomes))
        }
        Err(err) => {
            tracing::error!(%err, message = message.id, "queued message content is missing");
            None
        }
    };
    let (raw, outcomes) = match outcomes {
        Some((raw, outcomes)) => (raw, outcomes),
        None => {
            (Vec::new(), recipients.iter().map(|_| Outcome::Failed("the message content was lost".into())).collect())
        }
    };

    let mut failed = Vec::new();
    for (recipient, outcome) in recipients.iter().zip(outcomes) {
        let result = match &outcome {
            Outcome::Delivered(reply) => {
                tracing::info!(message = message.id, to = %recipient.address, %reply, "delivered");
                ctx.stats.record(DeliveryEvent::Delivered);
                ctx.store.mark_recipient_delivered(recipient.id, reply).await
            }
            Outcome::Deferred(error) => {
                ctx.stats.record(DeliveryEvent::Deferred);
                let next = now() + retry_delay(recipient.attempts + 1);
                if next > message.expires_at {
                    tracing::warn!(message = message.id, to = %recipient.address, %error, "giving up after retries");
                    failed.push(FailedRecipient { address: recipient.address.clone(), error: error.clone() });
                    ctx.store.mark_recipient_failed(recipient.id, error).await
                } else {
                    tracing::info!(message = message.id, to = %recipient.address, %error, "delivery deferred");
                    ctx.store.mark_recipient_deferred(recipient.id, error, next).await
                }
            }
            Outcome::Failed(error) => {
                tracing::warn!(message = message.id, to = %recipient.address, %error, "delivery failed");
                ctx.stats.record(DeliveryEvent::Failed);
                if recipient.notify_flags & RCPT_NOTIFY_NEVER == 0 {
                    failed.push(FailedRecipient { address: recipient.address.clone(), error: error.clone() });
                }
                ctx.store.mark_recipient_failed(recipient.id, error).await
            }
        };
        if let Err(err) = result {
            tracing::error!(%err, recipient = recipient.id, "updating the queue failed");
        }
    }

    {
        let mut inflight = ctx.inflight.lock().expect("inflight set poisoned");
        for recipient in &recipients {
            inflight.remove(&recipient.id);
        }
    }

    if !failed.is_empty() && !raw.is_empty() {
        dsn::bounce(ctx, &message.return_path, &raw, &failed).await;
    }
    if let Err(err) = ctx.store.complete_queue_message(message.id).await {
        tracing::error!(%err, message = message.id, "cleaning up the queue failed");
    }
}

enum Via {
    Mx,
    Route,
    Relay(RelayConfig),
}

struct Target {
    host: String,
    addrs: Vec<SocketAddr>,
    via: Via,
    /// The domain's MTA-STS policy is enforced: TLS with a valid certificate for `host`, or nothing.
    verified_tls: bool,
}

async fn lookup(host: &str, port: u16) -> Vec<SocketAddr> {
    match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => addrs.take(MAX_ADDRESSES_PER_HOST).collect(),
        Err(err) => {
            tracing::debug!(%host, %err, "host lookup failed");
            Vec::new()
        }
    }
}

async fn resolve_targets(ctx: &Context, domain: &str) -> Result<Vec<Target>, Outcome> {
    let live = ctx.live();
    if let Some(route) = live.delivery.routes.get(domain) {
        let (host, port) = route
            .rsplit_once(':')
            .and_then(|(host, port)| Some((host.trim_matches(['[', ']']).to_owned(), port.parse::<u16>().ok()?)))
            .ok_or_else(|| Outcome::Deferred(format!("the route for {domain} is not host:port")))?;
        let addrs = lookup(&host, port).await;
        return Ok(vec![Target { host, addrs, via: Via::Route, verified_tls: false }]);
    }
    if let Some(relay) = &live.delivery.relay {
        let addrs = lookup(&relay.host, relay.port).await;
        return Ok(vec![Target {
            host: relay.host.clone(),
            addrs,
            via: Via::Relay(relay.clone()),
            verified_tls: false,
        }]);
    }

    let auth = &ctx.authenticator;
    let (hosts, implicit) = match auth.mx_lookup(domain, Some(&ctx.dns.mx)).await {
        Ok(records) => {
            let hosts: Vec<String> = records
                .rrset
                .iter()
                .flat_map(|mx| mx.exchanges.iter())
                .map(|host| host.trim_end_matches('.').to_owned())
                .collect();
            if hosts.len() == 1 && hosts[0].is_empty() {
                return Err(Outcome::Failed(format!("556 5.1.10 {domain} does not accept mail (null MX)")));
            }
            if hosts.is_empty() { (vec![domain.to_owned()], true) } else { (hosts, false) }
        }
        Err(mail_auth::Error::Dns(DnsError::RecordNotFound(_))) => (vec![domain.to_owned()], true),
        Err(err) => return Err(Outcome::Deferred(format!("DNS lookup for {domain} failed: {err}"))),
    };

    // An enforced MTA-STS policy limits the hosts and requires TLS with a valid certificate.
    let enforced = mta_sts::policy_for(ctx, domain).await.filter(|policy| policy.mode == mta_sts::Mode::Enforce);
    let hosts: Vec<String> = match &enforced {
        Some(policy) => {
            let allowed: Vec<String> = hosts.into_iter().filter(|host| policy.allows(host)).collect();
            if allowed.is_empty() {
                return Err(Outcome::Deferred(format!(
                    "MTA-STS: the policy of {domain} allows none of its mail servers"
                )));
            }
            allowed
        }
        None => hosts,
    };

    let mut targets = Vec::new();
    for host in hosts.into_iter().take(MAX_HOSTS) {
        match auth
            .ip_lookup(
                &host,
                IpLookupStrategy::Ipv4thenIpv6,
                MAX_ADDRESSES_PER_HOST,
                Some(&ctx.dns.ipv4),
                Some(&ctx.dns.ipv6),
            )
            .await
        {
            Ok(ips) => {
                let addrs = ips.into_iter().map(|ip| SocketAddr::new(ip, live.delivery.mx_port)).collect();
                targets.push(Target { host, addrs, via: Via::Mx, verified_tls: enforced.is_some() });
            }
            Err(mail_auth::Error::Dns(DnsError::RecordNotFound(_))) if implicit => {
                return Err(Outcome::Failed(format!("550 5.1.2 The domain {domain} does not exist")));
            }
            Err(err) => tracing::debug!(%host, %err, "address lookup failed"),
        }
    }
    Ok(targets)
}

async fn deliver_domain(
    ctx: &Context,
    domain: &str,
    message: &QueuedMessage,
    raw: &[u8],
    recipients: &[QueueRecipient],
) -> Vec<Outcome> {
    let everyone = |outcome: Outcome| recipients.iter().map(|_| outcome.clone()).collect::<Vec<_>>();
    let targets = match resolve_targets(ctx, domain).await {
        Ok(targets) => targets,
        Err(outcome) => return everyone(outcome),
    };
    let mut last_error = format!("no mail server for {domain} could be reached");
    for target in &targets {
        for addr in &target.addrs {
            match session(ctx, target, *addr, message, raw, recipients).await {
                Ok(outcomes) => return outcomes,
                Err(error) => {
                    tracing::debug!(host = %target.host, %addr, %error, "delivery attempt failed");
                    last_error = format!("{} [{}]: {error}", target.host, addr.ip());
                }
            }
        }
    }
    let route = match targets.first().map(|target| &target.via) {
        Some(Via::Relay(_)) => Route::Relay,
        _ => ctx.direct_route(),
    };
    ctx.stats.trouble(domain, route, ProbeStage::Connect, last_error.clone());
    everyone(Outcome::Deferred(last_error))
}

/// One SMTP transaction. `Err` means nothing was accepted and another host may be tried.
async fn session(
    ctx: &Context,
    target: &Target,
    addr: SocketAddr,
    message: &QueuedMessage,
    raw: &[u8],
    recipients: &[QueueRecipient],
) -> Result<Vec<Outcome>, String> {
    let live = ctx.live();
    let settings = &live.delivery;
    let io = |err: std::io::Error| err.to_string();
    let everyone = |outcome: Outcome| recipients.iter().map(|_| outcome.clone()).collect::<Vec<_>>();
    let relay = match &target.via {
        Via::Relay(relay) => Some(relay),
        Via::Mx | Via::Route => None,
    };

    let mut client = Client::connect(
        ctx,
        addr,
        Duration::from_secs(settings.connect_timeout_secs),
        Duration::from_secs(settings.command_timeout_secs),
    )
    .await
    .map_err(io)?;
    if relay.is_some_and(|r| r.security == RelaySecurity::Tls) {
        client = client.tls_handshake(ctx.client_tls.verified.clone(), &target.host).await.map_err(io)?;
    }

    let greeting = client.read_reply().await.map_err(io)?;
    if greeting.code != 220 {
        return Err(format!("greeting: {greeting}"));
    }
    let (reply, mut caps) = client.ehlo(&ctx.hostname).await.map_err(io)?;
    if !reply.is_positive() {
        let helo = client.send(&format!("HELO {}\r\n", ctx.hostname)).await.map_err(io)?;
        if !helo.is_positive() {
            return Err(format!("HELO: {helo}"));
        }
    }

    let (want_tls, must_tls) = match relay {
        Some(relay) => (relay.security == RelaySecurity::Starttls, relay.security == RelaySecurity::Starttls),
        None => (true, settings.require_tls || target.verified_tls),
    };
    if want_tls && !client.is_tls() {
        if caps.starttls {
            let reply = client.send("STARTTLS\r\n").await.map_err(io)?;
            if reply.code == 220 {
                let config = if relay.is_some() || target.verified_tls {
                    ctx.client_tls.verified.clone()
                } else {
                    ctx.client_tls.opportunistic.clone()
                };
                client = client.tls_handshake(config, &target.host).await.map_err(|e| format!("TLS: {e}"))?;
                let (reply, new_caps) = client.ehlo(&ctx.hostname).await.map_err(io)?;
                if !reply.is_positive() {
                    return Err(format!("EHLO after STARTTLS: {reply}"));
                }
                caps = new_caps;
            } else if must_tls {
                return Err(format!("STARTTLS refused: {reply}"));
            }
        } else if must_tls {
            return Err("the server does not offer STARTTLS".into());
        }
    }

    if let Some(relay) = relay
        && let (Some(username), Some(password)) = (&relay.username, &relay.password)
    {
        let reply = client.auth_plain(username, password).await.map_err(io)?;
        if !reply.is_positive() {
            client.quit().await;
            let error = format!("the relay refused our login: {reply}");
            let domain = recipients.first().map(|recipient| recipient.domain.as_str()).unwrap_or_default();
            ctx.stats.trouble(domain, Route::Relay, ProbeStage::Login, error.clone());
            return Ok(everyone(Outcome::Deferred(error)));
        }
    }

    if let Some(limit) = caps.size
        && raw.len() > limit
    {
        client.quit().await;
        return Ok(everyone(Outcome::Failed(format!(
            "552 5.3.4 The message is too big for {} (limit {limit} bytes)",
            target.host
        ))));
    }

    let needs_utf8 = !message.return_path.is_ascii() || recipients.iter().any(|r| !r.address.is_ascii());
    if needs_utf8 && !caps.smtputf8 {
        client.quit().await;
        return Ok(everyone(Outcome::Failed(
            "553 5.6.7 The receiving server does not support international addresses".into(),
        )));
    }
    let mut mail_from = format!("MAIL FROM:<{}>", message.return_path);
    if caps.size.is_some() {
        mail_from.push_str(&format!(" SIZE={}", raw.len()));
    }
    if caps.eight_bit_mime && !raw.is_ascii() {
        mail_from.push_str(" BODY=8BITMIME");
    }
    if needs_utf8 {
        mail_from.push_str(" SMTPUTF8");
    }
    mail_from.push_str("\r\n");
    let reply = client.send(&mail_from).await.map_err(io)?;
    if !reply.is_positive() {
        client.quit().await;
        return Ok(everyone(Outcome::from_reply(&reply)));
    }

    let mut outcomes: Vec<Option<Outcome>> = vec![None; recipients.len()];
    let mut accepted = Vec::new();
    for (index, recipient) in recipients.iter().enumerate() {
        let reply = client.send(&format!("RCPT TO:<{}>\r\n", recipient.address)).await.map_err(io)?;
        if reply.is_positive() {
            accepted.push(index);
        } else {
            outcomes[index] = Some(Outcome::from_reply(&reply));
        }
    }

    if !accepted.is_empty() {
        let reply = client.send("DATA\r\n").await.map_err(io)?;
        let final_outcome = if reply.code != 354 {
            Outcome::from_reply(&reply)
        } else {
            let data_timeout = Duration::from_secs(settings.command_timeout_secs.max(60) * 2);
            match client.data(raw, data_timeout).await {
                Ok(reply) => Outcome::from_reply(&reply),
                // The message may or may not have arrived; retrying later is the safe choice.
                Err(err) => {
                    for index in accepted {
                        outcomes[index] = Some(Outcome::Deferred(format!("connection lost after sending: {err}")));
                    }
                    return Ok(outcomes.into_iter().map(|o| o.expect("every recipient has an outcome")).collect());
                }
            }
        };
        for index in accepted {
            outcomes[index] = Some(final_outcome.clone());
        }
    }
    client.quit().await;
    Ok(outcomes.into_iter().map(|o| o.expect("every recipient has an outcome")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_grow_and_cap() {
        assert_eq!(retry_delay(1), 300);
        assert_eq!(retry_delay(4), 3600);
        assert_eq!(retry_delay(40), 21_600);
    }
}
