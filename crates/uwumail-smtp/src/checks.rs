//! SPF, DKIM and DMARC verification of mail from other servers.

use std::net::IpAddr;

use mail_auth::common::headers::HeaderWriter;
use mail_auth::dmarc::Policy;
use mail_auth::dmarc::verify::DmarcParameters;
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{AuthenticatedMessage, AuthenticationResults, DkimResult, DmarcResult, SpfResult};

use crate::Context;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Accept,
    /// Deliver, but into Junk.
    Quarantine,
    /// Refuse with this reason.
    Reject(String),
}

#[derive(Debug, Clone)]
pub struct Verdict {
    /// Complete `Authentication-Results:` header including CRLF.
    pub header: String,
    pub action: Action,
    /// SPF or DKIM passed, so bouncing to the sender does not create backscatter.
    pub sender_verified: bool,
}

pub async fn verify(ctx: &Context, ip: IpAddr, helo: &str, mail_from: &str, raw: &[u8]) -> Verdict {
    let hostname = ctx.hostname.as_str();
    let Some(message) = AuthenticatedMessage::parse(raw) else {
        return Verdict {
            header: format!("Authentication-Results: {hostname}; none\r\n"),
            action: Action::Accept,
            sender_verified: false,
        };
    };

    let auth = &ctx.authenticator;
    let dkim = auth.verify_dkim(ctx.dns.params(&message)).await;
    let spf = auth.verify_spf(ctx.dns.params(SpfParameters::verify(ip, helo, hostname, mail_from))).await;
    let mail_from_domain = mail_from.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(helo);
    let dmarc = auth.verify_dmarc(ctx.dns.params(DmarcParameters::new(&message, &dkim, mail_from_domain, &spf))).await;

    let header_from = message.from.first().map(String::as_str).unwrap_or_default();
    let header = AuthenticationResults::new(hostname)
        .with_dkim_results(&dkim, header_from)
        .with_spf_mailfrom_result(&spf, ip, mail_from, helo)
        .with_dmarc_result(&dmarc)
        .to_header();

    // Both alignments failed against a published policy.
    let dmarc_failed =
        matches!(dmarc.dkim_result(), DmarcResult::Fail(_)) && matches!(dmarc.spf_result(), DmarcResult::Fail(_));
    let action = match (dmarc_failed, dmarc.policy()) {
        (true, Policy::Reject) if ctx.smtp.enforce_dmarc_reject => {
            Action::Reject(format!("the DMARC policy of {} rejects this message", dmarc.domain()))
        }
        (true, Policy::Reject | Policy::Quarantine) => Action::Quarantine,
        _ => Action::Accept,
    };

    let sender_verified =
        spf.result() == SpfResult::Pass || dkim.iter().any(|output| output.result() == &DkimResult::Pass);

    Verdict { header, action, sender_verified }
}
