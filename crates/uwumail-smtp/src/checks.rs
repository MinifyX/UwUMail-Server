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
    /// The address in the From header, normalized like the addresses on sender lists.
    pub from_address: Option<String>,
    /// SPF or DKIM passed for the From domain or a domain related to it, with or without a published
    /// DMARC policy. Only then does the From address say who really sent the message.
    pub from_verified: bool,
}

/// A verdict that refuses the message outright, before any SPF/DKIM/DMARC result. Used for a header
/// block that cannot be judged safely (more than one From, or a block a malformed line cut short).
fn rejecting(hostname: &str, reason: &str) -> Verdict {
    Verdict {
        header: format!("Authentication-Results: {hostname}; none\r\n"),
        action: Action::Reject(reason.to_string()),
        sender_verified: false,
        dmarc_passed: false,
        spf_failed: false,
        dkim_failed: false,
        dmarc_failed: true,
        from_domain: None,
        from_address: None,
        from_verified: false,
    }
}

pub async fn verify(ctx: &Context, ip: IpAddr, helo: &str, mail_from: &str, raw: &[u8]) -> Verdict {
    let hostname = ctx.hostname.as_str();
    // The From the recipient sees must be the one the checks below run against. A header block with
    // more than one From, or one a malformed line cut short so the real From hides below it, is
    // refused before it can be judged (security-audit-0.5.2 S-3/S-4).
    if let Some(reason) = crate::headers::header_block_fault(raw) {
        return rejecting(hostname, reason);
    }
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
            from_address: None,
            from_verified: false,
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

    // Verified enough that a bounce to the envelope sender is not backscatter: SPF passed for the
    // MAIL FROM domain (SPF is checked against exactly that), or a DKIM signature aligned with it
    // passed. A DKIM pass from an unrelated domain the attacker signs with does not count
    // (security-audit-0.5.2 S-15).
    let sender_verified = spf.result() == SpfResult::Pass
        || dkim.iter().any(|output| {
            output.result() == &DkimResult::Pass
                && output.signature().is_some_and(|signature| related_to_from(Some(mail_from), &signature.d))
        });

    let dmarc_passed =
        matches!(dmarc.dkim_result(), DmarcResult::Pass) || matches!(dmarc.spf_result(), DmarcResult::Pass);

    let spf_failed = spf.result() == SpfResult::Fail;
    let dkim_failed = dkim.iter().any(|output| matches!(output.result(), DkimResult::Fail(_)));
    let from_domain = header_from.rsplit_once('@').map(|(_, domain)| domain.trim().to_ascii_lowercase());
    let from_address = normalized_address(header_from);

    // DMARC only judges domains that publish a policy. Without one, a signature or SPF pass for the
    // From domain, a parent or a subdomain of it still shows who sent the message.
    let related = |domain: &str| related_to_from(from_address.as_deref(), domain);
    let from_verified = dmarc_passed
        || dkim.iter().any(|output| {
            output.result() == &DkimResult::Pass && output.signature().is_some_and(|signature| related(&signature.d))
        })
        || (spf.result() == SpfResult::Pass && related(mail_from_domain));

    Verdict {
        header,
        action,
        sender_verified,
        dmarc_passed,
        spf_failed,
        dkim_failed,
        dmarc_failed,
        from_domain,
        from_address,
        from_verified,
    }
}

/// Whether a domain is the From domain, a parent of it or below it.
fn related_to_from(from_address: Option<&str>, domain: &str) -> bool {
    let from = from_address.and_then(|address| address.rsplit_once('@')).map(|(_, domain)| domain);
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    from.is_some_and(|from| {
        domain.contains('.')
            && (from == domain || from.ends_with(&format!(".{domain}")) || domain.ends_with(&format!(".{from}")))
    })
}

/// What a message can still be asked when there is no sending server to ask: its signatures.
///
/// This is for mail fetched out of a mailbox somewhere else. A DKIM signature travels with the
/// message and still says who signed it, but SPF went with the connection that delivered it the
/// first time, and so did the envelope. So nothing here is ever called a failure that merely
/// travelling could have caused: no SPF result, no DMARC failure, no quarantine. What the provider
/// found out instead is read in [`crate::fetched`] and counts as points, not as a verdict.
pub async fn verify_signatures(ctx: &Context, raw: &[u8]) -> Verdict {
    let hostname = ctx.hostname.as_str();
    let mut verdict = Verdict {
        header: format!("Authentication-Results: {hostname}; none\r\n"),
        action: Action::Accept,
        sender_verified: false,
        dmarc_passed: false,
        spf_failed: false,
        dkim_failed: false,
        dmarc_failed: false,
        from_domain: None,
        from_address: None,
        from_verified: false,
    };
    let Some(message) = AuthenticatedMessage::parse(raw) else {
        return verdict;
    };

    let dkim = ctx.authenticator.verify_dkim(ctx.dns.params(&message)).await;
    let header_from = message.from.first().map(String::as_str).unwrap_or_default();
    verdict.header = AuthenticationResults::new(hostname).with_dkim_results(&dkim, header_from).to_header();
    verdict.from_domain = header_from.rsplit_once('@').map(|(_, domain)| domain.trim().to_ascii_lowercase());
    verdict.from_address = normalized_address(header_from);
    verdict.sender_verified = dkim.iter().any(|output| output.result() == &DkimResult::Pass);
    verdict.dkim_failed = dkim.iter().any(|output| matches!(output.result(), DkimResult::Fail(_)));
    // A signature that holds for the From domain is as good as it was before the message travelled:
    // it says the domain really sent this, which is all DMARC alignment asks of DKIM.
    let signed_by_sender = dkim.iter().any(|output| {
        output.result() == &DkimResult::Pass
            && output
                .signature()
                .is_some_and(|signature| related_to_from(verdict.from_address.as_deref(), &signature.d))
    });
    verdict.dmarc_passed = signed_by_sender;
    verdict.from_verified = signed_by_sender;
    verdict
}

/// An address as sender lists store it, or `None` if it is not one.
pub fn normalized_address(address: &str) -> Option<String> {
    uwumail_store::normalize_address(address).ok().map(|(local, domain)| format!("{local}@{domain}"))
}
