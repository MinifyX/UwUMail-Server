//! Refusals and throttling by Microsoft (Outlook.com, Hotmail, Exchange Online / Microsoft 365).
//!
//! Microsoft turns mail away for reasons of its own more often than other big providers: a sending
//! address on its block list (`S3150`), a whole range it does not take traffic from (`5.7.708`),
//! throttling while an address has little reputation (`4.7.650`), or a sender domain that does not
//! meet its authentication rules for bulk senders (`5.7.515`, since May 2025). The delivery worker
//! hands every answer of a Microsoft mail server to [`classify`], the store keeps what it finds as
//! issues per sending address or domain, and the portal explains them to the admins
//! (docs/microsoft.md). Bounces the sender gets say the same in plain words.

use std::net::IpAddr;

use serde::Serialize;

/// Where Microsoft's delisting form lives.
pub const DELIST_URL: &str = "https://sender.office.com";

/// Host names of Microsoft's mail servers: Exchange Online and Outlook.com (worldwide, the US
/// government cloud and the one run by 21Vianet in China).
const MICROSOFT_HOSTS: [&str; 5] =
    ["protection.outlook.com", "outlook.com", "hotmail.com", "protection.office365.us", "partner.outlook.cn"];

/// Whether a mail server host belongs to Microsoft.
pub fn is_microsoft_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    MICROSOFT_HOSTS.iter().any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// Whether a greeting comes from one of Microsoft's mail servers: they name themselves first
/// ("AM4PEPF00027A62.mail.protection.outlook.com Microsoft ESMTP MAIL Service ready"). Exchange
/// servers of other organisations say "Microsoft ESMTP" too, but not with such a name.
pub fn is_microsoft_greeting(text: &str) -> bool {
    text.split_whitespace().next().is_some_and(is_microsoft_host)
}

/// How bad it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueKind {
    /// Mail is refused.
    Blocked,
    /// Mail is accepted only slowly and deferred meanwhile.
    Throttled,
    /// The sender domain does not pass Microsoft's authentication rules.
    Authentication,
}

impl IssueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueKind::Blocked => "blocked",
            IssueKind::Throttled => "throttled",
            IssueKind::Authentication => "authentication",
        }
    }
}

/// What a refusal is about: the address mail leaves from, or the domain it is sent for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueScope {
    Ip,
    Domain,
}

impl IssueScope {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueScope::Ip => "ip",
            IssueScope::Domain => "domain",
        }
    }
}

/// What Microsoft's answer means, for the portal to explain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueGroup {
    /// `S3150`, `S3140`: the address (or its network) is on Microsoft's block list.
    BlockList,
    /// `5.7.511`, `5.7.606`: the address is banned.
    Banned,
    /// `5.7.708` and the rest of `5.7.6xx`–`5.7.7xx`: traffic from the address is not accepted.
    IpRefused,
    /// `4.7.650`, `4.7.500`: rate limited because of the address's reputation.
    Throttled,
    /// `5.7.515`: the sender domain does not meet the required authentication level.
    Authentication,
    /// `5.7.509`: DMARC fails and the domain's policy says reject.
    Dmarc,
}

impl IssueGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueGroup::BlockList => "blockList",
            IssueGroup::Banned => "banned",
            IssueGroup::IpRefused => "ipRefused",
            IssueGroup::Throttled => "throttled",
            IssueGroup::Authentication => "authentication",
            IssueGroup::Dmarc => "dmarc",
        }
    }

    pub fn parse(value: &str) -> Option<IssueGroup> {
        [
            IssueGroup::BlockList,
            IssueGroup::Banned,
            IssueGroup::IpRefused,
            IssueGroup::Throttled,
            IssueGroup::Authentication,
            IssueGroup::Dmarc,
        ]
        .into_iter()
        .find(|group| group.as_str() == value)
    }

    pub fn kind(self) -> IssueKind {
        match self {
            IssueGroup::BlockList | IssueGroup::Banned | IssueGroup::IpRefused => IssueKind::Blocked,
            IssueGroup::Throttled => IssueKind::Throttled,
            IssueGroup::Authentication | IssueGroup::Dmarc => IssueKind::Authentication,
        }
    }

    pub fn scope(self) -> IssueScope {
        match self {
            IssueGroup::Authentication | IssueGroup::Dmarc => IssueScope::Domain,
            _ => IssueScope::Ip,
        }
    }
}

/// One refusal or deferral Microsoft gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub group: IssueGroup,
    /// The code that names it: Microsoft's own (`S3150`) where there is one, else the enhanced
    /// status code (`5.7.708`).
    pub code: String,
    /// The address Microsoft says it saw, when the answer names one.
    pub ip: Option<IpAddr>,
}

/// Enhanced status codes (`5.7.708`) in a reply.
fn status_codes(text: &str) -> impl Iterator<Item = (u8, u16, u16)> + '_ {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '[' | ']')).filter_map(|token| {
        let mut parts = token.trim_end_matches(['.', ':']).split('.');
        let class = parts.next()?.parse::<u8>().ok().filter(|class| matches!(class, 2 | 4 | 5))?;
        let subject = parts.next()?;
        let detail = parts.next()?;
        if parts.next().is_some() || subject.len() > 3 || detail.len() > 3 {
            return None;
        }
        Some((class, subject.parse().ok()?, detail.parse().ok()?))
    })
}

/// Microsoft's own codes like `S3150` or `S775`.
fn s_code(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric()).find_map(|token| {
        let digits = token.strip_prefix('S')?;
        (3..=4).contains(&digits.len()).then_some(())?;
        digits.bytes().all(|b| b.is_ascii_digit()).then(|| token.to_owned())
    })
}

/// The first address in square brackets, which is where Microsoft names the sending address:
/// "messages from [203.0.113.5] weren't sent", "The mail server [203.0.113.5] has been
/// temporarily rate limited".
fn bracketed_ip(text: &str) -> Option<IpAddr> {
    text.split('[').skip(1).find_map(|rest| rest.split(']').next()?.trim().parse().ok())
}

/// Signs in the text itself that the answer came from Microsoft, for when the host is not known
/// (a bounce built from the stored error).
fn sounds_like_microsoft(lower: &str) -> bool {
    ["outlook.com", "hotmail.com", "microsoft", "exchangelabs", "office365", "outlook.cn", "live.com"]
        .iter()
        .any(|sign| lower.contains(sign))
}

/// What an answer of a Microsoft mail server means, if it is one of the refusals worth telling
/// the admins about. `from_microsoft` says the answer came from a Microsoft host; without that,
/// only answers that name Microsoft themselves count.
pub fn classify(reply: &str, from_microsoft: bool) -> Option<Refusal> {
    let lower = reply.to_ascii_lowercase();
    let s_code = s_code(reply);
    if !from_microsoft && !sounds_like_microsoft(&lower) {
        // Microsoft's block list codes are their own; nobody else writes S3150.
        if !matches!(s_code.as_deref(), Some("S3150" | "S3140" | "S3115")) {
            return None;
        }
    }
    let ip = bracketed_ip(reply);
    let codes: Vec<(u8, u16, u16)> = status_codes(reply).collect();
    let refusal = |group: IssueGroup, code: String| Some(Refusal { group, code, ip });
    let status = |(class, subject, detail): (u8, u16, u16)| format!("{class}.{subject}.{detail}");

    // Microsoft's own block list codes, whatever the status code around them.
    if let Some(code) = s_code.as_deref().filter(|code| matches!(*code, "S3150" | "S3140" | "S3115")) {
        return refusal(IssueGroup::BlockList, code.to_owned());
    }
    for &(class, subject, detail) in &codes {
        let code = status((class, subject, detail));
        match (class, subject, detail) {
            (4, 7, 650) | (4, 7, 500) => return refusal(IssueGroup::Throttled, code),
            (5, 7, 515) => return refusal(IssueGroup::Authentication, code),
            (5, 7, 509) => return refusal(IssueGroup::Dmarc, code),
            (5, 7, 511) | (5, 7, 606) => return refusal(IssueGroup::Banned, code),
            (5, 7, 600..=799) => return refusal(IssueGroup::IpRefused, code),
            _ => {}
        }
    }
    // A plain 5.7.1 only counts when it says the address is blocked; the same code also means
    // "this recipient does not take mail from outside", which is nobody's reputation.
    if codes.iter().any(|&(class, subject, detail)| (class, subject, detail) == (5, 7, 1))
        && ["block list", "blocklist", "blocked using", "banned sender", "blacklist"]
            .iter()
            .any(|words| lower.contains(words))
    {
        return refusal(IssueGroup::BlockList, "5.7.1".into());
    }
    if lower.contains("banned sender") {
        return refusal(
            IssueGroup::Banned,
            codes.first().map(|code| status(*code)).unwrap_or_else(|| "5.7.511".into()),
        );
    }
    // Throttling said in words, with only the basic code.
    if reply.starts_with('4') && (lower.contains("rate limited") || lower.contains("temporarily rate limit")) {
        return refusal(
            IssueGroup::Throttled,
            codes.first().map(|code| status(*code)).unwrap_or_else(|| "4.7.650".into()),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_hosts_are_known() {
        assert!(is_microsoft_host("example-org.mail.protection.outlook.com"));
        assert!(is_microsoft_host("hotmail-com.olc.protection.outlook.com."));
        assert!(is_microsoft_host("mx1.hotmail.com"));
        assert!(is_microsoft_host("example-org.mail.protection.office365.us"));
        assert!(!is_microsoft_host("mx.example.org"));
        assert!(!is_microsoft_host("notoutlook.com"));
        assert!(is_microsoft_greeting(
            "AM4PEPF00027A62.mail.protection.outlook.com Microsoft ESMTP MAIL Service ready"
        ));
        assert!(!is_microsoft_greeting("exchange.example.org Microsoft ESMTP MAIL Service ready"));
    }

    #[test]
    fn block_list_refusals() {
        let reply = "550 5.7.1 Unfortunately, messages from [203.0.113.5] weren't sent. Please contact your \
                     Internet service provider since part of their network is on our block list (S3150). \
                     [AM0PR01MB1234.eurprd01.prod.exchangelabs.com]";
        let refusal = classify(reply, true).unwrap();
        assert_eq!(refusal.group, IssueGroup::BlockList);
        assert_eq!(refusal.code, "S3150");
        assert_eq!(refusal.ip, Some("203.0.113.5".parse().unwrap()));
        assert_eq!(refusal.group.kind(), IssueKind::Blocked);
        assert_eq!(refusal.group.scope(), IssueScope::Ip);
        // The block list code is Microsoft's alone, so the stored text is enough.
        assert_eq!(classify("550 5.7.1 ... on our block list (S3140)", false).unwrap().code, "S3140");
    }

    #[test]
    fn refused_ranges_and_banned_senders() {
        let reply = "550 5.7.708 Service unavailable. Access denied, traffic not accepted from this IP. \
                     For more information please go to http://go.microsoft.com/fwlink/?LinkId=526653 AS(830)";
        let refusal = classify(reply, true).unwrap();
        assert_eq!((refusal.group, refusal.code.as_str(), refusal.ip), (IssueGroup::IpRefused, "5.7.708", None));
        let banned = classify("550 5.7.511 Access denied, banned sender[198.51.100.7]. To request removal", true);
        assert_eq!(banned.unwrap().group, IssueGroup::Banned);
        assert_eq!(
            classify("550 5.7.606 Access denied, banned sending IP [203.0.113.9]", true).unwrap().code,
            "5.7.606"
        );
        assert_eq!(
            classify("550 5.7.750 Service unavailable. Client blocked", true).unwrap().group,
            IssueGroup::IpRefused
        );
    }

    #[test]
    fn throttling() {
        let reply = "451 4.7.650 The mail server [203.0.113.5] has been temporarily rate limited due to IP \
                     reputation. For e-mail delivery information, see https://postmaster.live.com (S775) \
                     [DB5EUR03FT012.eop-EUR03.prod.protection.outlook.com]";
        let refusal = classify(reply, false).unwrap();
        assert_eq!(refusal.group, IssueGroup::Throttled);
        assert_eq!(refusal.code, "4.7.650");
        assert_eq!(refusal.group.kind(), IssueKind::Throttled);
        assert_eq!(classify("421 4.7.500 Server busy. Please try again later.", true).unwrap().code, "4.7.500");
    }

    #[test]
    fn authentication_rules() {
        let reply = "550 5.7.515 Access denied, sending domain [example.org] doesn't meet the required \
                     authentication level.";
        let refusal = classify(reply, true).unwrap();
        assert_eq!((refusal.group, refusal.group.scope()), (IssueGroup::Authentication, IssueScope::Domain));
        let dmarc = classify("550 5.7.509 Access denied, sending domain example.org does not pass DMARC", true);
        assert_eq!(dmarc.unwrap().group, IssueGroup::Dmarc);
    }

    #[test]
    fn ordinary_answers_are_not_issues() {
        // A recipient that does not exist, or one that takes no mail from outside, says nothing
        // about the server's reputation.
        assert_eq!(classify("550 5.5.0 Requested action not taken: mailbox unavailable", true), None);
        assert_eq!(classify("550 5.7.1 RESOLVER.RST.NotAuthorized; not authorized", true), None);
        assert_eq!(classify("250 2.6.0 Queued mail for delivery", true), None);
        // Another provider's 5.7.708 is not Microsoft's.
        assert_eq!(classify("550 5.7.708 whatever", false), None);
        assert_eq!(classify("451 4.7.650 slow down", false), None);
        // Unless the text says it is Microsoft.
        assert!(classify("451 4.7.650 rate limited [x.prod.protection.outlook.com]", false).is_some());
    }
}
