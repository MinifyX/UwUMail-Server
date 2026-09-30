//! The delivery worker: takes due messages from the queue and hands them to other servers.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use mail_auth::{DnsError, IpLookupStrategy};
use smtp_proto::RCPT_NOTIFY_NEVER;
use tokio::sync::watch;
use uwumail_store::{QueueRecipient, QueuedMessage};

use crate::client::{Client, Reply};
use crate::config::{RelayConfig, RelaySecurity};
use crate::dane::{self, Dane, Security, ValidatedMx};
use crate::dsn::{self, FailedRecipient};
use crate::health::{DeliveryEvent, ProbeStage, Route};
use crate::microsoft;
use crate::mta_sts;
use crate::tlsrpt::{self, PolicyType, ReportPolicy, ResultType};
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
                ctx.store.stats().count(uwumail_store::Stat::Delivered);
                ctx.store.mark_recipient_delivered(recipient.id, reply).await
            }
            Outcome::Deferred(error) => {
                ctx.stats.record(DeliveryEvent::Deferred);
                ctx.store.stats().count(uwumail_store::Stat::Deferred);
                let next = now() + retry_delay(recipient.attempts + 1);
                if next > message.expires_at {
                    tracing::warn!(message = message.id, to = %recipient.address, %error, "giving up after retries");
                    ctx.store.stats().count(uwumail_store::Stat::Bounced);
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
                ctx.store.stats().count(uwumail_store::Stat::Bounced);
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
    /// The domain's MTA-STS policy is being tested: certificate problems are reported, not enforced.
    sts_testing: bool,
    /// TLSA records of the host, which come before MTA-STS.
    dane: Dane,
    /// What the domain's TLS reports say about sessions to this host; only for MX hosts.
    report: Option<ReportPolicy>,
}

impl Target {
    fn elsewhere(host: String, addrs: Vec<SocketAddr>, via: Via) -> Target {
        Target { host, addrs, via, verified_tls: false, sts_testing: false, dane: Dane::Off, report: None }
    }
}

/// Watches one session for Microsoft's refusals (docs/microsoft.md): the first one becomes an
/// issue for the admins, and mail Microsoft accepted counts towards ending them.
struct MicrosoftWatch<'a> {
    ctx: &'a Context,
    /// The session is with a Microsoft mail server, by the host name we connected to.
    microsoft: bool,
    /// The address mail leaves from, when it is one the internet sees.
    local_ip: Option<IpAddr>,
    sender_domain: String,
    noted: bool,
}

impl<'a> MicrosoftWatch<'a> {
    fn new(ctx: &'a Context, target: &Target, local_ip: Option<IpAddr>, return_path: &str) -> MicrosoftWatch<'a> {
        let relay = matches!(target.via, Via::Relay(_));
        MicrosoftWatch {
            ctx,
            microsoft: !relay && microsoft::is_microsoft_host(&target.host),
            local_ip: local_ip.filter(|ip| !ip.is_unspecified() && !crate::servercheck::is_private(*ip)),
            sender_domain: return_path
                .rsplit_once('@')
                .map(|(_, domain)| domain.to_ascii_lowercase())
                .unwrap_or_default(),
            // A relay answers for itself; what Microsoft says to it comes back as a bounce.
            noted: relay,
        }
    }

    async fn refused(&mut self, reply: &Reply) {
        // Only a session with a Microsoft host becomes an issue. The greeting and the reply text
        // are whatever the other side chooses to say: any mail server could greet as
        // "….protection.outlook.com" and answer S3150 to raise a false alarm for the admins.
        if self.noted || !self.microsoft || reply.is_positive() {
            return;
        }
        let text = reply.to_string();
        let Some(refusal) = microsoft::classify(&text, true) else { return };
        self.noted = true;
        let ours = match refusal.ip {
            Some(said) if Some(said) != self.local_ip => {
                lookup(&self.ctx.hostname, 25).await.into_iter().map(|addr| addr.ip()).collect()
            }
            _ => Vec::new(),
        };
        let ip = refusal_ip(refusal.ip, self.local_ip, &ours).map(|ip| ip.to_string()).unwrap_or_default();
        let scope = refusal.group.scope();
        let subject = match scope {
            microsoft::IssueScope::Ip => ip.clone(),
            microsoft::IssueScope::Domain => self.sender_domain.clone(),
        };
        tracing::warn!(code = %refusal.code, %ip, group = refusal.group.as_str(), %text, "Microsoft refused mail");
        let recorded = self
            .ctx
            .store
            .record_microsoft_refusal(
                uwumail_store::MicrosoftRefusal {
                    scope: scope.as_str(),
                    subject,
                    group: refusal.group.as_str(),
                    code: refusal.code,
                    ip,
                    domain: self.sender_domain.clone(),
                    reply: text,
                },
                now(),
            )
            .await;
        if let Err(err) = recorded {
            tracing::error!(%err, "recording a refusal by Microsoft failed");
        }
    }

    async fn delivered(&self) {
        if !self.microsoft {
            return;
        }
        let ip = self.local_ip.map(|ip| ip.to_string());
        if let Err(err) = self.ctx.store.record_microsoft_delivery(ip, self.sender_domain.clone(), now()).await {
            tracing::error!(%err, "recording a delivery to Microsoft failed");
        }
    }
}

/// The address a refusal is about: the one Microsoft names, but only when it is one of ours (the
/// address this session left from, or one our host name points to), else the address the session
/// left from. Behind NAT only the reply knows the public address, but an address that is not ours
/// would make an issue about somebody else's.
fn refusal_ip(said: Option<IpAddr>, local_ip: Option<IpAddr>, ours: &[IpAddr]) -> Option<IpAddr> {
    said.filter(|ip| Some(*ip) == local_ip || ours.contains(ip)).or(local_ip)
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

/// Mail from a fetched address leaves through that provider's own outgoing server, whoever it is
/// addressed to. Sent from here it would carry our name on the envelope and theirs in the From
/// header, and their DMARC policy would take it apart at the recipient.
async fn sender_route(ctx: &Context, account_id: Option<i64>, return_path: &str) -> Result<Option<Target>, Outcome> {
    // The route hangs on the sending account, not on the envelope address alone: mail with no
    // account (bounces, system mail) never takes a fetched sender's server.
    let Some(account_id) = account_id else { return Ok(None) };
    if return_path.is_empty() {
        return Ok(None);
    }
    let mut sender = match ctx.store.fetch_sender(account_id, return_path).await {
        Ok(Some(sender)) => sender,
        Ok(None) => return Ok(None),
        Err(err) => {
            tracing::warn!(%err, "looking up the outgoing server of a fetched address failed");
            return Ok(None);
        }
    };
    // A mailbox that signs in at Microsoft or Google sends with an access token. Without one the
    // message waits: going out any other way would be the very thing the provider's DMARC refuses.
    if sender.auth.is_oauth() {
        match ctx.provider_oauth.access_token(&ctx.store, sender.account_id, sender.fetch_id, &sender.address).await {
            Ok(token) => sender.password = token,
            Err(err) => return Err(Outcome::Deferred(format!("sending as {}: {err}", sender.address))),
        }
    }
    let relay = RelayConfig {
        host: sender.host,
        port: sender.port,
        security: match sender.security {
            uwumail_store::SendSecurity::Tls => RelaySecurity::Tls,
            uwumail_store::SendSecurity::Starttls => RelaySecurity::Starttls,
        },
        username: Some(sender.username),
        password: Some(sender.password),
        oauth: sender.auth.is_oauth(),
    };
    // Only public addresses: a fetched account's outgoing server must not point the delivery worker
    // at this host or the local network, even when a public-looking name resolves there
    // (security-audit-0.5.2 S-10). With none left the target is unreachable and the message defers.
    let addrs: Vec<SocketAddr> =
        lookup(&relay.host, relay.port).await.into_iter().filter(|a| crate::fetch::is_public(a.ip())).collect();
    if addrs.is_empty() {
        tracing::warn!(host = %relay.host, "the outgoing server of a fetched address is not a public host");
    }
    Ok(Some(Target::elsewhere(relay.host.clone(), addrs, Via::Relay(relay))))
}

async fn resolve_targets(
    ctx: &Context,
    domain: &str,
    account_id: Option<i64>,
    return_path: &str,
) -> Result<Vec<Target>, Outcome> {
    let live = ctx.live();
    if let Some(route) = live.delivery.routes.get(domain) {
        let (host, port) = route
            .rsplit_once(':')
            .and_then(|(host, port)| Some((host.trim_matches(['[', ']']).to_owned(), port.parse::<u16>().ok()?)))
            .ok_or_else(|| Outcome::Deferred(format!("the route for {domain} is not host:port")))?;
        let addrs = lookup(&host, port).await;
        return Ok(vec![Target::elsewhere(host, addrs, Via::Route)]);
    }
    // Before the server's own smarthost: whose account it comes from, together with the address,
    // decides where it may leave.
    if let Some(target) = sender_route(ctx, account_id, return_path).await? {
        return Ok(vec![target]);
    }
    if let Some(relay) = &live.delivery.relay {
        let addrs = lookup(&relay.host, relay.port).await;
        return Ok(vec![Target::elsewhere(relay.host.clone(), addrs, Via::Relay(relay.clone()))]);
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
            if hosts.is_empty() { (vec![domain.to_owned()], true) } else { (hosts, false) }
        }
        Err(mail_auth::Error::Dns(DnsError::RecordNotFound(_))) => (vec![domain.to_owned()], true),
        Err(err) => return Err(Outcome::Deferred(format!("DNS lookup for {domain} failed: {err}"))),
    };

    // DANE needs MX records that validate, and then the hosts they name, not the ones an ordinary
    // resolver answered: that answer, or its "there are none", is what an attacker on the path would
    // forge to lead the mail to a host without TLSA records (RFC 7672, section 2.2.1).
    let validated = dane::validated_mx(ctx, domain).await;
    let mx_security = validated.security();
    if mx_security == Security::Bogus {
        let report = ReportPolicy {
            kind: PolicyType::Tlsa,
            strings: Vec::new(),
            mx: Vec::new(),
            failure: Some(ResultType::DnssecInvalid),
        };
        tlsrpt::record(ctx, domain, &report, None, "", None, None).await;
        return Err(Outcome::Deferred(format!("DNSSEC: the MX records of {domain} do not validate")));
    }
    let (hosts, implicit) = match validated {
        ValidatedMx::Secure(validated) => (validated.to_vec(), false),
        _ => (hosts, implicit),
    };
    if hosts.len() == 1 && hosts[0].is_empty() {
        return Err(Outcome::Failed(format!("556 5.1.10 {domain} does not accept mail (null MX)")));
    }

    // An enforced MTA-STS policy limits the hosts and requires TLS with a valid certificate.
    let sts = mta_sts::policy_for(ctx, domain).await;
    let policy = sts.policy.filter(|policy| policy.mode != mta_sts::Mode::None);
    let enforced = policy.as_ref().is_some_and(|policy| policy.mode == mta_sts::Mode::Enforce);
    let hosts: Vec<String> = match &policy {
        Some(policy) if enforced => {
            let allowed: Vec<String> = hosts.into_iter().filter(|host| policy.allows(host)).collect();
            if allowed.is_empty() {
                return Err(Outcome::Deferred(format!(
                    "MTA-STS: the policy of {domain} allows none of its mail servers"
                )));
            }
            allowed
        }
        _ => hosts,
    };

    // With validated MX records, the TLSA records of their hosts come before MTA-STS.
    let mut targets = Vec::new();
    let (mut looked_up, mut private) = (0, 0);
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
                looked_up += 1;
                let found = !ips.is_empty();
                // Whoever controls a domain's DNS could otherwise point its MX at this host or the
                // local network and have the delivery worker talk to services there. A mail
                // server that really lives there gets a route (or `delivery.allow_private_mx`).
                let addrs: Vec<SocketAddr> = ips
                    .into_iter()
                    .filter(|ip| live.delivery.allow_private_mx || crate::fetch::is_public(*ip))
                    .map(|ip| SocketAddr::new(ip, live.delivery.mx_port))
                    .collect();
                if found && addrs.is_empty() {
                    private += 1;
                    tracing::warn!(%domain, %host, "an MX host has no public address, skipping it");
                    continue;
                }
                let dane = match mx_security {
                    Security::Secure => Dane::of(dane::host_tlsa(ctx, &host).await),
                    _ => Dane::Off,
                };
                let report = ReportPolicy::of(&host, &dane, policy.as_ref(), sts.failure);
                targets.push(Target {
                    verified_tls: enforced && !dane.applies(),
                    sts_testing: policy.is_some() && !enforced && !dane.applies(),
                    host,
                    addrs,
                    via: Via::Mx,
                    dane,
                    report: Some(report),
                });
            }
            Err(mail_auth::Error::Dns(DnsError::RecordNotFound(_))) if implicit => {
                return Err(Outcome::Failed(format!("550 5.1.2 The domain {domain} does not exist")));
            }
            Err(err) => tracing::debug!(%host, %err, "address lookup failed"),
        }
    }
    // Only when every host pointed inside: a host whose lookup failed may still come back.
    if targets.is_empty() && private > 0 && private == looked_up {
        return Err(Outcome::Failed(format!(
            "550 5.4.4 The mail servers of {domain} point to private addresses, not to the internet"
        )));
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
    let targets = match resolve_targets(ctx, domain, message.account_id, &message.return_path).await {
        Ok(targets) => targets,
        Err(outcome) => return everyone(outcome),
    };
    let mut last_error = format!("no mail server for {domain} could be reached");
    for target in &targets {
        if target.dane == Dane::Bogus {
            // RFC 7672, section 2.2: an MX host whose TLSA records do not validate is not used.
            if let Some(report) = &target.report {
                tlsrpt::record(ctx, domain, report, None, &target.host, None, None).await;
            }
            last_error = format!("{}: DNSSEC: its TLSA records do not validate", target.host);
            continue;
        }
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

    let mut microsoft_watch = MicrosoftWatch::new(ctx, target, client.local_ip(), &message.return_path);
    let greeting = client.read_reply().await.map_err(io)?;
    if greeting.code != 220 {
        microsoft_watch.refused(&greeting).await;
        return Err(format!("greeting: {greeting}"));
    }
    let (reply, mut caps) = client.ehlo(&ctx.hostname).await.map_err(io)?;
    if !reply.is_positive() {
        let helo = client.send(&format!("HELO {}\r\n", ctx.hostname)).await.map_err(io)?;
        if !helo.is_positive() {
            return Err(format!("HELO: {helo}"));
        }
    }

    // A relay set to "none" still uses STARTTLS opportunistically when the relay offers it, so the
    // AUTH login and the mail are not sent in the clear against a relay that supports TLS -- it
    // just is not required or verified, matching "only on your own network" (S-31).
    let (want_tls, must_tls) = match relay {
        Some(relay) => (
            matches!(relay.security, RelaySecurity::Starttls | RelaySecurity::None),
            relay.security == RelaySecurity::Starttls,
        ),
        None => (true, settings.require_tls || target.verified_tls || target.dane.applies()),
    };
    // Sessions to MX hosts count for the domain's TLS reports, once it is clear how TLS went.
    let domain = recipients.first().map(|recipient| recipient.domain.as_str()).unwrap_or_default();
    let local_ip = client.local_ip();
    let report = |result: Option<ResultType>| async move {
        if let Some(policy) = &target.report {
            tlsrpt::record(ctx, domain, policy, result, &target.host, Some(addr.ip()), local_ip).await;
        }
    };
    if want_tls && !client.is_tls() {
        if caps.starttls {
            let reply = client.send("STARTTLS\r\n").await.map_err(io)?;
            if reply.code == 220 {
                let mut testing = None;
                // A "none" relay uses the non-verifying config: it may carry a self-signed
                // certificate on the local network, so encrypt without demanding a valid one.
                let config = match &target.dane {
                    Dane::Verify(records) if relay.is_none() => {
                        ctx.client_tls.dane(records.clone(), &[target.host.as_str(), domain])?
                    }
                    _ if relay.is_some_and(|r| r.security != RelaySecurity::None) || target.verified_tls => {
                        ctx.client_tls.verified.clone()
                    }
                    _ if target.sts_testing => {
                        let (config, verifier) = ctx.client_tls.report_only()?;
                        testing = Some(verifier);
                        config
                    }
                    _ => ctx.client_tls.opportunistic.clone(),
                };
                client = match client.tls_handshake(config, &target.host).await {
                    Ok(client) => client,
                    Err(e) => {
                        report(Some(crate::tls::result_type(&e))).await;
                        return Err(format!("TLS: {e}"));
                    }
                };
                let problem = testing.and_then(|verifier| verifier.problem());
                report(problem.as_ref().map(crate::tls::certificate_result)).await;
                let (reply, new_caps) = client.ehlo(&ctx.hostname).await.map_err(io)?;
                if !reply.is_positive() {
                    return Err(format!("EHLO after STARTTLS: {reply}"));
                }
                caps = new_caps;
            } else {
                report(Some(ResultType::StarttlsNotSupported)).await;
                if must_tls {
                    return Err(format!("STARTTLS refused: {reply}"));
                }
            }
        } else {
            report(Some(ResultType::StarttlsNotSupported)).await;
            if must_tls {
                return Err("the server does not offer STARTTLS".into());
            }
        }
    }

    if let Some(relay) = relay
        && let (Some(username), Some(password)) = (&relay.username, &relay.password)
    {
        let reply = if relay.oauth {
            client.auth_xoauth2(username, password).await
        } else {
            client.auth_plain(username, password).await
        }
        .map_err(io)?;
        if !reply.is_positive() {
            client.quit().await;
            let error = format!("the relay refused our login: {reply}");
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
        microsoft_watch.refused(&reply).await;
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
            microsoft_watch.refused(&reply).await;
            outcomes[index] = Some(Outcome::from_reply(&reply));
        }
    }

    if !accepted.is_empty() {
        let reply = client.send("DATA\r\n").await.map_err(io)?;
        let final_outcome = if reply.code != 354 {
            microsoft_watch.refused(&reply).await;
            Outcome::from_reply(&reply)
        } else {
            let data_timeout = Duration::from_secs(settings.command_timeout_secs.max(60) * 2);
            match client.data(raw, data_timeout).await {
                Ok(reply) => {
                    if reply.is_positive() {
                        microsoft_watch.delivered().await;
                    } else {
                        microsoft_watch.refused(&reply).await;
                    }
                    Outcome::from_reply(&reply)
                }
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

    #[test]
    fn a_refusal_names_only_our_own_addresses() {
        let local: IpAddr = "203.0.113.5".parse().unwrap();
        let ours: IpAddr = "198.51.100.7".parse().unwrap();
        let foreign: IpAddr = "192.0.2.9".parse().unwrap();
        // Someone else's address in the reply: the address the session left from.
        assert_eq!(refusal_ip(Some(foreign), Some(local), &[ours]), Some(local));
        assert_eq!(refusal_ip(Some(foreign), None, &[ours]), None);
        // Ours, by the session or by the host name (behind NAT).
        assert_eq!(refusal_ip(Some(local), Some(local), &[]), Some(local));
        assert_eq!(refusal_ip(Some(ours), None, &[ours]), Some(ours));
        assert_eq!(refusal_ip(None, Some(local), &[]), Some(local));
    }
}
