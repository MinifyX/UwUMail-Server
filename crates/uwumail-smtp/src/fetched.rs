//! Mail this server takes out of a mailbox at another provider.
//!
//! A fetched message has already been delivered once, so the checks that judge a sending server
//! have nothing left to look at: the connection was to the provider's server, not to the one that
//! sent the mail, and the envelope is gone. Without a stand-in, every fetched message would arrive
//! either unjudged or wrongly judged, and both are worse than not fetching at all.
//!
//! What this does instead:
//!
//! * **Check DKIM ourselves.** A signature travels with the message and does not care how many
//!   mailboxes it passed through, so this is the one check that is worth as much as it was before.
//! * **Read what the provider found out**, from the `Authentication-Results` header it wrote. That
//!   header counts only when it carries the provider's own name, because anyone can write one.
//!   When it names the address the message came from, this server checks that address itself, and
//!   from there on the message is judged exactly like one handed in at the door.
//! * **Believe the rest only against a message, never for it.** A `Received-SPF` line nobody signed
//!   for, a spam flag from the provider, the fact that it lay in the provider's junk folder: all of
//!   these can add points, none of them can take any away. Forging them only hurts the forger.
//!
//! The junk folder is fetched like any other, and this server judges its mail again from scratch:
//! the provider's verdict is one rule among many, not the answer.

use std::net::IpAddr;

use crate::headers;
use crate::spam::links;

/// The provider said so with its own name on it.
pub const PROVIDER_SPF_FAIL: f32 = 2.0;
pub const PROVIDER_DKIM_FAIL: f32 = 1.0;
pub const PROVIDER_DMARC_FAIL: f32 = 2.5;
/// Nothing vouches for the message: no signature of ours held, and the provider says nothing.
pub const FETCHED_NO_AUTH: f32 = 1.0;
/// It lay in the provider's junk folder.
pub const PROVIDER_JUNK: f32 = 2.5;
/// The provider marked it as spam in the headers.
pub const PROVIDER_SPAM_FLAG: f32 = 2.0;
/// The fetched address is in neither `To` nor `Cc`.
pub const NOT_ADDRESSED: f32 = 0.5;

/// The mailbox a message was taken from.
#[derive(Debug, Clone)]
pub struct Mailbox {
    pub id: i64,
    /// Whose mailbox here the mail goes into.
    pub account_id: i64,
    /// The address at the provider.
    pub address: String,
    /// The provider's server, for recognizing the headers it writes.
    pub host: String,
    /// The name the provider signs its `Authentication-Results` with. Empty derives it from `host`.
    pub auth_serv_id: String,
}

impl Mailbox {
    /// The name this server expects on the provider's own headers.
    fn authserv(&self) -> String {
        if !self.auth_serv_id.is_empty() {
            return self.auth_serv_id.clone();
        }
        links::site(&self.host)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    Pass,
    Fail,
    Other,
}

impl AuthResult {
    fn parse(value: &str) -> AuthResult {
        match value.trim().to_ascii_lowercase().as_str() {
            "pass" => AuthResult::Pass,
            // Softfail is a maybe by the domain's own wish, so it is not held against the message.
            "fail" | "permerror" => AuthResult::Fail,
            _ => AuthResult::Other,
        }
    }
}

/// What the provider wrote under its own name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attested {
    pub spf: Option<AuthResult>,
    pub dkim: Option<AuthResult>,
    pub dmarc: Option<AuthResult>,
    /// The address the message reached the provider from.
    pub client_ip: Option<IpAddr>,
    /// The name that server greeted the provider with.
    pub helo: Option<String>,
}

/// Everything this server could find out about where a fetched message has been.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    /// The mailbox it was taken from, so the history can tell fetched mail apart later.
    pub address: String,
    /// It lay in the provider's junk folder.
    pub from_junk: bool,
    /// The provider marked it as spam in the headers, and which header said so.
    pub spam_flag: Option<String>,
    /// What the provider put its name to.
    pub attested: Option<Attested>,
    /// An SPF result nobody signed for. Only counted when it speaks against the message.
    pub claimed_spf_fail: bool,
    /// The fetched address stands in `To` or `Cc`.
    pub addressed: bool,
}

/// `name=value` pairs of one segment of an `Authentication-Results` header.
fn parts(segment: &str) -> impl Iterator<Item = (&str, &str)> {
    segment.split_whitespace().filter_map(|part| {
        let (name, value) = part.trim_end_matches(';').split_once('=')?;
        Some((name.trim(), value.trim()))
    })
}

/// The provider's own `Authentication-Results`, or nothing.
///
/// Only the topmost one is read, and only when it carries the provider's name: the provider writes
/// its header above everything the message brought along, so anything below it may be a forgery.
fn attested(raw: &[u8], authserv: &str) -> Option<Attested> {
    let (fields, _) = headers::split(raw);
    let header = fields.iter().find(|field| field.name.eq_ignore_ascii_case("Authentication-Results"))?;
    let value = header.value();
    let (id, results) = value.split_once(';')?;
    let id = id.split_whitespace().next().unwrap_or_default().trim().to_ascii_lowercase();
    if !(id == authserv || id.ends_with(&format!(".{authserv}"))) {
        return None;
    }

    let mut found = Attested::default();
    for segment in results.split(';') {
        let mut pairs = parts(segment);
        let Some((method, result)) = pairs.next() else { continue };
        let result = AuthResult::parse(result);
        match method.to_ascii_lowercase().as_str() {
            "spf" => found.spf = Some(result),
            // Several signatures can be reported; one that holds is enough.
            "dkim" => {
                if found.dkim != Some(AuthResult::Pass) {
                    found.dkim = Some(result);
                }
            }
            "dmarc" => found.dmarc = Some(result),
            _ => {}
        }
        for (name, value) in pairs {
            match name.to_ascii_lowercase().as_str() {
                "client-ip" | "smtp.remote-ip" | "sender-ip" => {
                    found.client_ip = value.trim_matches('"').parse().ok();
                }
                "smtp.helo" | "helo" => found.helo = Some(value.trim_matches('"').to_owned()),
                _ => {}
            }
        }
    }
    Some(found)
}

/// Whether the provider marked the message as spam in the headers, and with which one.
fn spam_flag(raw: &[u8]) -> Option<String> {
    let yes = |value: &str| {
        let value = value.trim().to_ascii_lowercase();
        value.starts_with("yes") || value.starts_with("true") || value == "spam"
    };
    for name in ["X-Spam-Flag", "X-Spam", "X-Spam-Status"] {
        if let Some(value) = headers::first_value(raw, name)
            && yes(&value)
        {
            return Some(name.to_owned());
        }
    }
    None
}

/// Whether the fetched address stands in `To` or `Cc`. Mail that names somebody else was either
/// sent to a hidden list of addresses or forwarded, both of which say little on their own.
fn addressed_to(raw: &[u8], address: &str) -> bool {
    let address = address.to_ascii_lowercase();
    ["To", "Cc"]
        .iter()
        .any(|name| headers::first_value(raw, name).is_some_and(|value| value.to_ascii_lowercase().contains(&address)))
}

/// The envelope sender the provider recorded, so the message keeps the address bounces would go to.
pub fn return_path(raw: &[u8]) -> Option<String> {
    let value = headers::first_value(raw, "Return-Path")?;
    let value = value.trim().trim_start_matches('<').trim_end_matches('>').trim();
    // An empty return path is a bounce and stays empty.
    if value.is_empty() || !value.contains('@') { None } else { Some(value.to_owned()) }
}

/// Reads everything the headers of a fetched message say about where it has been.
pub fn read(mailbox: &Mailbox, from_junk: bool, raw: &[u8]) -> Provenance {
    Provenance {
        address: mailbox.address.clone(),
        from_junk,
        spam_flag: spam_flag(raw),
        attested: attested(raw, &mailbox.authserv()),
        claimed_spf_fail: headers::first_value(raw, "Received-SPF").is_some_and(|value| {
            AuthResult::parse(value.split_whitespace().next().unwrap_or_default()) == AuthResult::Fail
        }),
        addressed: addressed_to(raw, &mailbox.address),
    }
}

impl Provenance {
    /// The address to check ourselves, when the provider named one under its own name.
    pub fn checkable_client(&self) -> Option<(IpAddr, String)> {
        let attested = self.attested.as_ref()?;
        let ip = attested.client_ip?;
        Some((ip, attested.helo.clone().unwrap_or_default()))
    }

    /// Whether anything at all vouches for this message.
    fn vouched_for(&self, own_dkim_passed: bool) -> bool {
        own_dkim_passed
            || self.attested.as_ref().is_some_and(|attested| {
                [attested.spf, attested.dkim, attested.dmarc].iter().flatten().any(|r| *r == AuthResult::Pass)
            })
    }

    /// The rules a fetched message answers to. `checked_ourselves` says whether this server could
    /// verify the sending address itself; then SPF, DKIM and DMARC have already been scored the
    /// usual way and the provider's word adds nothing.
    pub fn rules(&self, checked_ourselves: bool, own_dkim_passed: bool) -> Vec<(&'static str, f32, Option<String>)> {
        // Worth no points, but it says in the message's own headers and in the history that this
        // one was fetched, and out of which mailbox. Without it, a fetched message that nothing at
        // all was wrong with would be indistinguishable from one handed in at the door.
        let mut hits: Vec<(&'static str, f32, Option<String>)> = vec![("FETCHED", 0.0, Some(self.address.clone()))];
        if self.from_junk {
            hits.push(("PROVIDER_JUNK", PROVIDER_JUNK, None));
        }
        if let Some(header) = &self.spam_flag {
            hits.push(("PROVIDER_SPAM_FLAG", PROVIDER_SPAM_FLAG, Some(header.clone())));
        }
        if !self.addressed {
            hits.push(("NOT_ADDRESSED", NOT_ADDRESSED, None));
        }
        if checked_ourselves {
            return hits;
        }
        if let Some(attested) = &self.attested {
            if attested.spf == Some(AuthResult::Fail) {
                hits.push(("PROVIDER_SPF_FAIL", PROVIDER_SPF_FAIL, None));
            }
            if attested.dkim == Some(AuthResult::Fail) {
                hits.push(("PROVIDER_DKIM_FAIL", PROVIDER_DKIM_FAIL, None));
            }
            if attested.dmarc == Some(AuthResult::Fail) {
                hits.push(("PROVIDER_DMARC_FAIL", PROVIDER_DMARC_FAIL, None));
            }
        } else if self.claimed_spf_fail {
            // Nobody signed for this one, so it only counts because it counts against the message.
            hits.push(("PROVIDER_SPF_FAIL", PROVIDER_SPF_FAIL, Some("Received-SPF".into())));
        }
        if !self.vouched_for(own_dkim_passed) {
            hits.push(("FETCHED_NO_AUTH", FETCHED_NO_AUTH, None));
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox() -> Mailbox {
        Mailbox {
            id: 1,
            account_id: 1,
            address: "mini@icloud.example".into(),
            host: "imap.mail.icloud.example".into(),
            auth_serv_id: String::new(),
        }
    }

    fn message(headers: &str) -> Vec<u8> {
        format!("{headers}\r\nSubject: hello\r\n\r\nbody\r\n").replace('\n', "\r\n").replace("\r\r", "\r").into_bytes()
    }

    #[test]
    fn the_providers_own_header_is_read_and_a_strangers_is_not() {
        let raw = message(
            "Authentication-Results: mx.icloud.example; spf=pass smtp.mailfrom=shop@shop.example client-ip=203.0.113.9; dkim=pass header.d=shop.example; dmarc=pass\r\n\
             To: mini@icloud.example",
        );
        let found = read(&mailbox(), false, &raw).attested.expect("the provider's own name is on it");
        assert_eq!(found.spf, Some(AuthResult::Pass));
        assert_eq!(found.dkim, Some(AuthResult::Pass));
        assert_eq!(found.dmarc, Some(AuthResult::Pass));
        assert_eq!(found.client_ip, Some("203.0.113.9".parse().unwrap()));

        // The same header under somebody else's name says nothing at all.
        let forged = message("Authentication-Results: mx.somewhere.example; spf=pass; dmarc=pass");
        assert_eq!(read(&mailbox(), false, &forged).attested, None);
    }

    #[test]
    fn only_the_topmost_header_counts() {
        let raw = message(
            "Authentication-Results: mx.icloud.example; spf=fail\r\n\
             Authentication-Results: mx.icloud.example; spf=pass",
        );
        let found = read(&mailbox(), false, &raw).attested.unwrap();
        assert_eq!(found.spf, Some(AuthResult::Fail), "the provider writes above what the message brought");
    }

    #[test]
    fn what_nobody_signed_for_can_only_count_against_the_message() {
        let failing = message("Received-SPF: Fail (example.com does not allow this server) client-ip=203.0.113.9");
        let prov = read(&mailbox(), false, &failing);
        assert!(prov.claimed_spf_fail);
        assert!(prov.checkable_client().is_none(), "an unsigned address is never checked as if it were true");
        let rules: Vec<&str> = prov.rules(false, false).iter().map(|(rule, _, _)| *rule).collect();
        assert!(rules.contains(&"PROVIDER_SPF_FAIL"));

        let passing = message("Received-SPF: Pass (example.com allows this server) client-ip=203.0.113.9");
        let prov = read(&mailbox(), false, &passing);
        assert!(!prov.claimed_spf_fail);
        let rules: Vec<&str> = prov.rules(false, false).iter().map(|(rule, _, _)| *rule).collect();
        assert!(rules.contains(&"FETCHED_NO_AUTH"), "a claimed pass vouches for nothing");
        assert!(!rules.contains(&"PROVIDER_SPF_FAIL"));
    }

    #[test]
    fn the_junk_folder_and_a_spam_flag_add_points_but_do_not_decide() {
        let raw = message("X-Spam-Flag: YES\r\nTo: mini@icloud.example");
        let prov = read(&mailbox(), true, &raw);
        let rules = prov.rules(false, true);
        let points: f32 = rules.iter().map(|(_, points, _)| points).sum();
        let names: Vec<&str> = rules.iter().map(|(rule, _, _)| *rule).collect();
        assert!(names.contains(&"PROVIDER_JUNK") && names.contains(&"PROVIDER_SPAM_FLAG"));
        let bar = crate::config::SpamConfig::default().junk_score;
        assert!(points < bar, "the provider's verdict alone does not fill the bar");
    }

    #[test]
    fn our_own_checks_replace_the_providers_word() {
        let raw = message(
            "Authentication-Results: mx.icloud.example; spf=fail; dkim=fail; dmarc=fail\r\nTo: mini@icloud.example",
        );
        let prov = read(&mailbox(), false, &raw);
        let rules = prov.rules(true, true);
        let names: Vec<&str> = rules.iter().map(|(rule, _, _)| *rule).collect();
        assert_eq!(names, ["FETCHED"], "when this server checked the sender itself, nothing is counted twice");
        assert_eq!(rules[0].1, 0.0, "and saying where it came from costs nothing");
        assert_eq!(rules[0].2.as_deref(), Some("mini@icloud.example"));
    }

    #[test]
    fn mail_addressed_to_somebody_else_is_noted() {
        let to_me = message("To: Mini <MINI@icloud.example>");
        assert!(read(&mailbox(), false, &to_me).addressed);
        let to_others = message("To: undisclosed-recipients:;");
        let prov = read(&mailbox(), false, &to_others);
        assert!(!prov.addressed);
        let names: Vec<&str> = prov.rules(true, true).iter().map(|(rule, _, _)| *rule).collect();
        assert!(names.contains(&"NOT_ADDRESSED"));
    }

    #[test]
    fn the_envelope_sender_comes_back_from_the_return_path() {
        assert_eq!(return_path(&message("Return-Path: <shop@shop.example>")).as_deref(), Some("shop@shop.example"));
        assert_eq!(return_path(&message("Return-Path: <>")), None, "a bounce keeps its empty envelope");
        assert_eq!(return_path(&message("Subject: none")), None);
    }
}
