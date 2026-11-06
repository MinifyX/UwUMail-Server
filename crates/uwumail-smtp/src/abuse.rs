//! Keeping our own people from hurting the server's name (docs/outbound-abuse.md): strikes that
//! suspend an account's sending for a while, complaints other providers send to `abuse@` (ARF,
//! RFC 5965, Microsoft's JMRP among them), and the host names whose addresses the login block
//! leaves alone.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::{Name, RData, RecordType};
use mail_auth::report::{Feedback, FeedbackType};
use mail_parser::MessageParser;
use tokio::sync::watch;
use uwumail_store::{NewAbuseReport, StrikeKind, StrikeLimits};

use crate::reports::{AT_ONCE, MAX_REPORT_BYTES};
use crate::{Context, Smtp, headers};

/// How often the exempt host names are looked up again. Home connections change their address
/// about once a day; five minutes keeps an admin locked out for at most that long after a change.
const RESOLVE_EVERY: Duration = Duration::from_secs(300);
/// The most addresses one exempt host name may stand for, so a name somebody else controls cannot
/// open the login block for a whole range.
const MAX_HOST_ADDRESSES: usize = 16;
/// Report senders whose complaints count for any recipient: Microsoft's JMRP reports for all of
/// Outlook.com, Hotmail, Live and MSN from this one address, which DMARC keeps others from using.
const TRUSTED_FEEDBACK_LOOPS: &[&str] = &["staff@hotmail.com"];

/// Counts a strike against an account and, when it was one too many, suspends its sending. Errors
/// are only logged: whatever caused the strike has been dealt with already.
pub(crate) async fn strike(ctx: &Context, account_id: i64, kind: StrikeKind) {
    let smtp = &ctx.live().smtp;
    let limits = StrikeLimits {
        strikes: smtp.suspend_after_strikes,
        bounces: smtp.suspend_after_hard_bounces,
        window_secs: i64::from(smtp.strike_window_minutes) * 60,
        suspend_secs: i64::from(smtp.suspend_minutes) * 60,
    };
    match ctx.store.record_send_strike(account_id, kind, crate::now(), limits).await {
        Ok(Some(suspension)) => tracing::warn!(
            login = %suspension.login,
            reason = %suspension.reason,
            until = suspension.until,
            "suspended sending to other servers for an account"
        ),
        Ok(None) => tracing::info!(account_id, kind = kind.as_str(), "counted a sending strike"),
        Err(err) => tracing::error!(%err, account_id, "counting a sending strike failed"),
    }
}

/// Whether mail for `address` goes to the abuse desk of one of our domains (RFC 2142).
pub(crate) fn is_abuse_address(address: &str) -> bool {
    address.split_once('@').is_some_and(|(local, _)| local.eq_ignore_ascii_case("abuse"))
}

/// Reads a message for `abuse@` in the background, in case it is a complaint report. The message
/// is delivered like any other either way; this only takes note. Shares the report readers' limits.
pub(crate) fn receive_soon(ctx: Arc<Context>, address: String, envelope: String, raw: &[u8], authenticated: bool) {
    if raw.len() > MAX_REPORT_BYTES {
        return;
    }
    let Ok(permit) = ctx.reports.clone().try_acquire_owned() else {
        tracing::warn!(%address, "{AT_ONCE} reports are being read already; a complaint is let go");
        return;
    };
    let raw = raw.to_vec();
    tokio::spawn(async move {
        receive(&ctx, address, envelope, raw, authenticated).await;
        drop(permit);
    });
}

async fn receive(ctx: &Context, address: String, envelope: String, raw: Vec<u8>, authenticated: bool) {
    let Ok(Some(complaint)) = tokio::task::spawn_blocking(move || complaint(&raw)).await else {
        return;
    };
    let domain = address.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase()).unwrap_or_default();
    let (account_id, recipient_domains) = sender_of(ctx, &complaint).await;
    let trusted = complaint.reporter.as_deref().is_some_and(|r| TRUSTED_FEEDBACK_LOOPS.contains(&r));
    let got_it = trusted || may_complain(complaint.reporter.as_deref(), &recipient_domains);
    let window = i64::from(ctx.live().smtp.strike_window_minutes) * 60;
    let report = NewAbuseReport {
        received_at: crate::now(),
        domain: domain.clone(),
        reporter: complaint.reporter.clone().unwrap_or(envelope),
        authenticated,
        feedback_type: complaint.feedback_type.to_owned(),
        user_agent: complaint.user_agent.clone(),
        source_ip: complaint.source_ip.map(|ip| ip.to_string()),
        original_message_id: complaint.message_id.clone(),
        original_from: complaint.from.clone(),
        original_subject: complaint.subject.clone(),
        account_id,
        may_count: got_it && complaint.about_spam,
        once_per_reporter_since: (!trusted).then(|| crate::now() - window),
    };
    match ctx.store.add_abuse_report(report).await {
        Ok((_, true)) => {
            tracing::warn!(%domain, reporter = ?complaint.reporter, "a provider complained about mail an account sent");
            if let Some(account_id) = account_id {
                strike(ctx, account_id, StrikeKind::Complaint).await;
            }
        }
        Ok((_, false)) => tracing::info!(%domain, "stored a complaint report"),
        Err(err) => tracing::error!(%err, %domain, "storing a complaint report failed"),
    }
}

/// Whether `reporter` speaks for one of the domains the mail went to. Only someone the mail went
/// to may complain about it (or a feedback loop we trust to speak for its users): anyone else could
/// collect our people's mail and report it until they are suspended (security review 0.28.0 OA-01).
fn may_complain(reporter: Option<&str>, recipient_domains: &[String]) -> bool {
    reporter
        .and_then(|r| r.rsplit_once('@'))
        .is_some_and(|(_, reporter)| recipient_domains.iter().any(|domain| crate::forward::aligned(reporter, domain)))
}

/// What a complaint report says about the message it complains about.
#[derive(Debug, Default)]
struct Complaint {
    feedback_type: &'static str,
    /// abuse, fraud or virus: the recipient did not want the mail. "not-spam" and authentication
    /// failure reports are kept for the admins but say nothing bad about the sender.
    about_spam: bool,
    /// Who sent the report (`From`), e.g. Microsoft's `staff@hotmail.com`.
    reporter: Option<String>,
    user_agent: Option<String>,
    source_ip: Option<IpAddr>,
    message_id: Option<String>,
    from: Option<String>,
    subject: Option<String>,
    /// The `d=` of every DKIM signature on the original.
    dkim_domains: Vec<String>,
}

/// Reads an ARF report, or `None` when `raw` is not one. The recipient of the original is never
/// read: Microsoft's JMRP removes it, and nothing here depends on whom the mail went to.
fn complaint(raw: &[u8]) -> Option<Complaint> {
    let feedback = Feedback::parse_rfc5322(raw).ok()?;
    let (feedback_type, about_spam) = match feedback.feedback_type() {
        FeedbackType::Abuse => ("abuse", true),
        FeedbackType::Fraud => ("fraud", true),
        FeedbackType::Virus => ("virus", true),
        FeedbackType::AuthFailure => ("auth-failure", false),
        FeedbackType::NotSpam => ("not-spam", false),
        FeedbackType::Other => ("other", false),
    };
    let report = MessageParser::new().parse_headers(raw);
    let mut complaint = Complaint {
        feedback_type,
        about_spam,
        reporter: report.as_ref().and_then(|report| first_address(report.from())),
        user_agent: feedback.user_agent().map(str::to_owned),
        source_ip: feedback.source_ip(),
        ..Complaint::default()
    };
    // The whole original (`message/rfc822`) or only its headers (`text/rfc822-headers`).
    let Some(original) = feedback.message().or(feedback.headers()) else { return Some(complaint) };
    let original = original.as_bytes();
    if let Some(parsed) = MessageParser::new().parse_headers(original) {
        complaint.from = first_address(parsed.from());
        complaint.subject = parsed.subject().map(str::to_owned);
    }
    complaint.message_id = headers::first_value(original, "Message-ID")
        .map(|id| id.trim().trim_matches(['<', '>']).to_owned())
        .filter(|id| !id.is_empty());
    complaint.dkim_domains =
        headers::values(original, "DKIM-Signature").iter().filter_map(|v| dkim_domain(v)).collect();
    Some(complaint)
}

fn first_address(address: Option<&mail_parser::Address<'_>>) -> Option<String> {
    address?.first()?.address.as_deref().map(str::to_ascii_lowercase)
}

/// The `d=` tag of a DKIM-Signature value, lower case.
fn dkim_domain(value: &str) -> Option<String> {
    value.split(';').find_map(|tag| {
        let (name, value) = tag.split_once('=')?;
        let domain: String = value.chars().filter(|c| !c.is_whitespace()).collect();
        (name.trim().eq_ignore_ascii_case("d") && !domain.is_empty()).then(|| domain.to_ascii_lowercase())
    })
}

/// The account that sent the original, and the domains it went to when that is known.
///
/// The Message-ID is looked up among what our people sent: it names the account and its
/// recipients. Failing that, a From address of ours with a DKIM signature of its domain names the
/// account for the admins, but never counts: anyone who got a signed message from us (a vacation
/// reply will do) could report it again and again. Microsoft edits the original (the recipient is
/// cut out), which usually breaks the signature anyway, so for JMRP the Message-ID is what counts.
async fn sender_of(ctx: &Context, complaint: &Complaint) -> (Option<i64>, Vec<String>) {
    if let Some(id) = &complaint.message_id
        && let Ok(Some((account_id, domains))) = ctx.store.sent_message_account(id).await
    {
        return (Some(account_id), domains);
    }
    let Some(from) = &complaint.from else { return (None, Vec::new()) };
    let Some((_, domain)) = from.rsplit_once('@') else { return (None, Vec::new()) };
    if !complaint.dkim_domains.iter().any(|d| d == domain) || !ctx.store.is_local_domain(domain).await.unwrap_or(false)
    {
        return (None, Vec::new());
    }
    (ctx.store.address_owner(from).await.ok().flatten(), Vec::new())
}

/// Looks the exempt host names of the login block up again every few minutes, and at once when the
/// list changes. A name that does not resolve keeps the addresses it had: a DNS hiccup must not
/// lock the admin out. Runs until `shutdown` changes.
pub async fn run_exempt_resolver(smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
    let ctx = smtp.inner.clone();
    loop {
        for (host, _) in ctx.store.block_exempt_hosts() {
            let found = host_addresses(ctx.authenticator.resolver(), &host).await;
            if found.is_empty() {
                tracing::debug!(%host, "an exempt host name did not resolve; keeping its last addresses");
                continue;
            }
            if let Err(err) = ctx.store.set_exempt_host_addresses(&host, found).await {
                tracing::error!(%err, %host, "noting the addresses of an exempt host name failed");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(RESOLVE_EVERY) => {}
            _ = ctx.store.exempt_hosts_changed() => {}
            _ = shutdown.changed() => break,
        }
    }
}

/// The A and AAAA addresses of `host`, through the server's resolver.
async fn host_addresses(resolver: &TokioResolver, host: &str) -> Vec<IpAddr> {
    let Ok(name) = Name::from_ascii(format!("{host}.")) else { return Vec::new() };
    let mut found = Vec::new();
    for record_type in [RecordType::A, RecordType::AAAA] {
        if let Ok(lookup) = resolver.lookup(name.clone(), record_type).await {
            found.extend(lookup.answers().iter().filter_map(|record| match &record.data {
                RData::A(a) => Some(IpAddr::V4(a.0)),
                RData::AAAA(aaaa) => Some(IpAddr::V6(aaaa.0)),
                _ => None,
            }));
        }
    }
    capped(found)
}

/// Distinct, public addresses, at most [`MAX_HOST_ADDRESSES`] of them. Whoever controls the name
/// must not be able to exempt loopback, the local network or the proxy in front of us (security
/// review 0.28.0 OA-05); a network of our own goes into the list as a network.
fn capped(addresses: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut kept: Vec<IpAddr> = Vec::new();
    for ip in addresses.into_iter().map(|ip| ip.to_canonical()) {
        if is_public(ip) && !kept.contains(&ip) && kept.len() < MAX_HOST_ADDRESSES {
            kept.push(ip);
        }
    }
    kept
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let shared = v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64; // 100.64.0.0/10
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || shared)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || first & 0xfe00 == 0xfc00 // unique local
                || first & 0xffc0 == 0xfe80) // link local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like a Microsoft JMRP report: sent by the provider's staff address, the original
    /// attached whole, its recipient cut out.
    fn jmrp(original_type: &str, original: &str) -> Vec<u8> {
        format!(
            "From: staff@fbl.example.net\r\n\
             To: abuse@uwu.test\r\n\
             Subject: complaint about message from 192.0.2.10\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/report; report-type=feedback-report; boundary=\"b1\"\r\n\
             \r\n\
             --b1\r\n\
             Content-Type: text/plain\r\n\
             \r\n\
             This is an email abuse report for an email message received from IP 192.0.2.10.\r\n\
             --b1\r\n\
             Content-Type: message/feedback-report\r\n\
             \r\n\
             Feedback-Type: abuse\r\n\
             User-Agent: Hotmail FBL\r\n\
             Version: 0.1\r\n\
             Source-IP: 192.0.2.10\r\n\
             Arrival-Date: Fri, 06 Nov 2026 10:00:00 +0000\r\n\
             \r\n\
             --b1\r\n\
             Content-Type: {original_type}\r\n\
             \r\n\
             {original}\r\n\
             --b1--\r\n"
        )
        .into_bytes()
    }

    const ORIGINAL: &str = "DKIM-Signature: v=1; a=rsa-sha256; d=UWU.test; s=uwu; h=from:subject; bh=x; b=y\r\n\
        From: Mini <mini@uwu.test>\r\n\
        To: \r\n\
        Subject: Big news\r\n\
        Message-ID: <abc123@uwu.test>\r\n\
        \r\n\
        Hello there.\r\n";

    #[test]
    fn reads_a_jmrp_report_with_the_original_attached() {
        let complaint = complaint(&jmrp("message/rfc822", ORIGINAL)).expect("a report");
        assert_eq!(complaint.feedback_type, "abuse");
        assert!(complaint.about_spam);
        assert_eq!(complaint.reporter.as_deref(), Some("staff@fbl.example.net"));
        assert_eq!(complaint.user_agent.as_deref(), Some("Hotmail FBL"));
        assert_eq!(complaint.source_ip, Some("192.0.2.10".parse().unwrap()));
        assert_eq!(complaint.message_id.as_deref(), Some("abc123@uwu.test"));
        assert_eq!(complaint.from.as_deref(), Some("mini@uwu.test"));
        assert_eq!(complaint.subject.as_deref(), Some("Big news"));
        assert_eq!(complaint.dkim_domains, ["uwu.test"]);
    }

    #[test]
    fn reads_a_report_with_only_the_headers() {
        let headers = ORIGINAL.split("\r\n\r\n").next().unwrap();
        let complaint = complaint(&jmrp("text/rfc822-headers", headers)).expect("a report");
        assert_eq!(complaint.message_id.as_deref(), Some("abc123@uwu.test"));
        assert_eq!(complaint.from.as_deref(), Some("mini@uwu.test"));
    }

    #[test]
    fn other_mail_and_garbage_are_no_reports() {
        assert!(complaint(b"From: a@example.com\r\nSubject: hi\r\n\r\nPlease stop.\r\n").is_none());
        assert!(complaint(b"").is_none());
        assert!(complaint(&[0xff; 64]).is_none());
        let broken = jmrp("message/rfc822", "\u{0}\u{0}garbage without headers");
        assert!(complaint(&broken).is_some_and(|c| c.message_id.is_none() && c.from.is_none()));
        let not_spam =
            String::from_utf8(jmrp("message/rfc822", ORIGINAL)).unwrap().replace("abuse\r\n", "not-spam\r\n");
        assert!(complaint(not_spam.as_bytes()).is_some_and(|c| !c.about_spam));
    }

    #[test]
    fn only_the_recipients_side_may_complain() {
        let domains = ["mail.example.net".to_owned(), "example.org".to_owned()];
        assert!(may_complain(Some("fbl@example.net"), &domains));
        assert!(may_complain(Some("staff@fbl.example.org"), &domains));
        assert!(!may_complain(Some("someone@example.com"), &domains));
        assert!(!may_complain(None, &domains));
        assert!(!may_complain(Some("fbl@example.net"), &[]));
    }

    #[test]
    fn abuse_addresses_and_dkim_domains() {
        assert!(is_abuse_address("Abuse@uwu.test"));
        assert!(!is_abuse_address("abuser@uwu.test"));
        assert_eq!(dkim_domain("v=1; d = Example.ORG ; s=x").as_deref(), Some("example.org"));
        assert_eq!(dkim_domain("v=1; s=x"), None);
    }

    #[test]
    fn host_addresses_are_distinct_and_capped() {
        let mut found: Vec<IpAddr> = (1..=40).map(|n| format!("192.0.2.{n}").parse().unwrap()).collect();
        found.insert(0, "::ffff:192.0.2.1".parse().unwrap());
        for local in
            ["0.0.0.0", "127.0.0.1", "10.0.0.1", "192.168.1.1", "172.17.0.1", "100.64.0.1", "::1", "fd00::1", "fe80::1"]
        {
            found.insert(0, local.parse().unwrap());
        }
        let kept = capped(found);
        assert_eq!(kept.len(), MAX_HOST_ADDRESSES);
        assert_eq!(kept[0], "192.0.2.1".parse::<IpAddr>().unwrap());
        assert_eq!(kept[1], "192.0.2.2".parse::<IpAddr>().unwrap());
    }
}
