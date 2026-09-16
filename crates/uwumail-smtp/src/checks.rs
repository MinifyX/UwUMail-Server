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
    /// SPF or DKIM passed aligned with the From domain.
    pub dmarc_passed: bool,
    /// SPF says this server may not send for the envelope sender.
    pub spf_failed: bool,
    /// A DKIM signature was there and did not hold.
    pub dkim_failed: bool,
    /// The From domain publishes DMARC and neither SPF nor DKIM passed aligned with it.
    pub dmarc_failed: bool,
    /// The domain in the From header, lowercase.
    pub from_domain: Option<String>,
}

pub async fn verify(ctx: &Context, ip: IpAddr, helo: &str, mail_from: &str, raw: &[u8]) -> Verdict {
    let hostname = ctx.hostname.as_str();
    let Some(message) = AuthenticatedMessage::parse(raw) else {
        return Verdict {
            header: format!("Authentication-Results: {hostname}; none\r\n"),
            action: Action::Accept,
            sender_verified: false,
            dmarc_passed: false,
            spf_failed: false,
            dkim_failed: false,
            dmarc_failed: false,
            from_domain: None,
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

    // A policy is published and neither SPF nor DKIM passed aligned with it. The single results
    // only say Fail when a check passed for another domain; a forgery whose SPF fails and that
    // carries no valid signature has None for both, and only the overall result calls that a fail.
    let dmarc_failed = matches!(dmarc.result(), DmarcResult::Fail(_));
    let action = match (dmarc_failed, dmarc.policy()) {
        (true, Policy::Reject) if ctx.live().smtp.enforce_dmarc_reject => {
            Action::Reject(format!("the DMARC policy of {} rejects this message", dmarc.domain()))
        }
        (true, Policy::Reject | Policy::Quarantine) => Action::Quarantine,
        _ => Action::Accept,
    };

    let sender_verified =
        spf.result() == SpfResult::Pass || dkim.iter().any(|output| output.result() == &DkimResult::Pass);

    let dmarc_passed =
        matches!(dmarc.dkim_result(), DmarcResult::Pass) || matches!(dmarc.spf_result(), DmarcResult::Pass);

    let spf_failed = spf.result() == SpfResult::Fail;
    let dkim_failed = dkim.iter().any(|output| matches!(output.result(), DkimResult::Fail(_)));
    let from_domain = header_from.rsplit_once('@').map(|(_, domain)| domain.trim().to_ascii_lowercase());

    Verdict { header, action, sender_verified, dmarc_passed, spf_failed, dkim_failed, dmarc_failed, from_domain }
}
