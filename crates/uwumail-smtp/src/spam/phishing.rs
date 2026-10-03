//! Deterministic phishing checks: what a message pretends to be, against what it is.
//!
//! Phishing rarely trips the classic spam rules: it comes from a fresh domain that authenticates
//! fine, with a short, polite text. What gives it away is a mismatch — a familiar name on an
//! unfamiliar address, a domain that only looks like a brand's or like a business partner's, a link
//! whose text names another site than the one it leads to. Those are facts that can be checked
//! without a network and without a model, so both the spam filter and the AI spam check use them.
//!
//! The brand list is small and curated on purpose: the brands German and international phishing
//! imitates most, with the domains they really send from. A brand that is missing costs a missed
//! hint; a wrong own-domain would cost a false alarm on every real mail of that brand, so own domains
//! are matched generously (the brand's name under a common ending) and lookalikes strictly.

use std::net::IpAddr;

use mail_parser::{Message, PartType};
use uwumail_store::mime_limits::parse_message;

use super::links::{self, Link, Target};
use super::{html, links::site};

/// One mismatch found, with the points the spam filter gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub rule: &'static str,
    pub points: f32,
    /// What exactly was seen, e.g. `paypa1-login.example looks like PayPal`.
    pub detail: String,
}

/// A brand phishing likes to imitate.
#[derive(Debug, Clone, Copy)]
pub struct Brand {
    /// How people write it, also what a display name is matched against (whole words).
    pub names: &'static [&'static str],
    /// The first label of its domains (`paypal` of `paypal.com`): what a lookalike imitates. Under
    /// a common ending ([`COMMON_ENDINGS`]) such a domain counts as the brand's own.
    pub labels: &'static [&'static str],
    /// Further domains (sites) the brand really sends from.
    pub sites: &'static [&'static str],
    /// The names are distinctive words. A brand whose name is also an ordinary word ("Apple",
    /// "Booking", "Steam") only counts in a display name when the name is all of it, or followed by
    /// a word like "Service" or "Support".
    pub distinct: bool,
    /// Every label that starts with the brand's label and a hyphen counts as its own, for brands with
    /// many regional domains (`sparkasse-musterstadt.de`) — only under the endings these banks
    /// really use ([`PREFIX_ENDINGS`]). Anybody can register `sparkasse-login.com`.
    pub own_prefix: bool,
}

const fn brand(
    names: &'static [&'static str],
    labels: &'static [&'static str],
    sites: &'static [&'static str],
) -> Brand {
    Brand { names, labels, sites, distinct: true, own_prefix: false }
}

const fn common(
    names: &'static [&'static str],
    labels: &'static [&'static str],
    sites: &'static [&'static str],
) -> Brand {
    Brand { names, labels, sites, distinct: false, own_prefix: false }
}

const fn regional(
    names: &'static [&'static str],
    labels: &'static [&'static str],
    sites: &'static [&'static str],
) -> Brand {
    Brand { names, labels, sites, distinct: true, own_prefix: true }
}

/// The brands that are checked. Real domains are product data here, not examples.
pub const BRANDS: &[Brand] = &[
    brand(&["PayPal"], &["paypal"], &["paypal-communication.com", "paypal-community.com", "paypalobjects.com"]),
    brand(&["Amazon", "Amazon Prime"], &["amazon"], &["amazonses.com", "amazonaws.com", "primevideo.com"]),
    common(&["Apple", "Apple ID", "iCloud", "App Store", "iTunes"], &["apple", "icloud"], &["me.com", "mac.com"]),
    brand(
        &["Microsoft", "Microsoft 365", "Office 365", "Outlook", "OneDrive", "SharePoint", "Microsoft Teams"],
        &["microsoft", "office365", "outlook", "onedrive", "sharepoint"],
        &[
            "office.com",
            "live.com",
            "hotmail.com",
            "msn.com",
            "microsoftonline.com",
            "sharepointonline.com",
            "onmicrosoft.com",
            "azure.com",
            "microsoft365.com",
        ],
    ),
    brand(&["Google", "Gmail", "Google Drive"], &["google", "gmail"], &["googlemail.com", "youtube.com"]),
    brand(&["Netflix"], &["netflix"], &["netflix.net"]),
    brand(&["Spotify"], &["spotify"], &[]),
    brand(&["DHL", "DHL Paket", "Deutsche Post"], &["dhl", "deutschepost"], &["dpdhl.com", "dhl-news.com"]),
    brand(&["DPD"], &["dpd"], &[]),
    brand(&["FedEx"], &["fedex"], &[]),
    common(&["UPS"], &["ups"], &[]),
    common(&["Hermes"], &["myhermes", "hermesworld"], &["hermes-europe.eu"]),
    regional(&["Sparkasse"], &["sparkasse"], &["s-cloud.de", "sparkasse.net"]),
    regional(&["Volksbank", "Raiffeisenbank", "VR-Bank", "VR Bank"], &["volksbank"], &["vr.de"]),
    brand(&["Deutsche Bank"], &["deutsche-bank", "deutschebank"], &["db.com"]),
    brand(&["Commerzbank"], &["commerzbank"], &[]),
    brand(&["comdirect"], &["comdirect"], &[]),
    brand(&["Postbank"], &["postbank"], &[]),
    common(&["ING", "ING-DiBa"], &["ing", "ing-diba"], &[]),
    brand(&["DKB"], &["dkb"], &[]),
    brand(&["N26"], &["n26"], &[]),
    brand(&["Revolut"], &["revolut"], &[]),
    brand(&["Klarna"], &["klarna"], &[]),
    brand(&["eBay"], &["ebay"], &[]),
    brand(&["Kleinanzeigen"], &["kleinanzeigen"], &[]),
    brand(&["Telekom", "Deutsche Telekom", "T-Online"], &["telekom", "t-online"], &[]),
    brand(&["Vodafone"], &["vodafone"], &[]),
    common(&["O2", "o2"], &["o2online"], &["o2.de", "telefonica.de"]),
    brand(&["1&1", "IONOS"], &["1und1", "ionos"], &[]),
    brand(&["GMX"], &["gmx"], &["web.de"]),
    brand(&["Facebook"], &["facebook", "facebookmail"], &["fb.com", "meta.com", "metamail.com"]),
    brand(&["Instagram"], &["instagram"], &[]),
    brand(&["WhatsApp"], &["whatsapp"], &[]),
    brand(&["LinkedIn"], &["linkedin"], &["licdn.com"]),
    common(&["Steam", "Steam Support"], &["steampowered", "steamcommunity"], &[]),
    common(&["Booking.com"], &["booking"], &[]),
    brand(&["Zalando"], &["zalando"], &[]),
    brand(&["DocuSign"], &["docusign"], &["docusign.net"]),
    brand(&["Dropbox"], &["dropbox"], &["dropboxmail.com"]),
    brand(&["Adobe"], &["adobe"], &[]),
    brand(&["Coinbase"], &["coinbase"], &[]),
    brand(&["Binance"], &["binance"], &[]),
    brand(&["Mastercard"], &["mastercard"], &[]),
    common(&["Visa"], &["visa"], &[]),
    brand(&["American Express"], &["americanexpress", "amex"], &["aexp.com"]),
    brand(&["ELSTER"], &["elster"], &["bzst.de"]),
    brand(&["Nespresso"], &["nespresso"], &[]),
    brand(&["Lidl"], &["lidl"], &[]),
    brand(&["ALDI", "ALDI SÜD", "ALDI Nord"], &["aldi", "aldi-sued", "aldi-nord"], &[]),
    brand(&["ADAC"], &["adac"], &[]),
    brand(&["IKEA"], &["ikea"], &[]),
    brand(&["MediaMarkt"], &["mediamarkt"], &[]),
];

/// Endings under which a brand's label is the brand's own domain.
const COMMON_ENDINGS: &[&str] = &[
    "com", "de", "at", "ch", "net", "org", "eu", "co.uk", "fr", "it", "es", "nl", "be", "lu", "pl", "se", "dk", "no",
    "fi", "ie", "pt", "cz", "ca", "us", "com.au", "co.jp", "com.br", "com.mx", "com.tr", "in", "co.in",
];

/// Endings under which a regional brand's prefixed labels (`sparkasse-musterstadt`) count as its
/// own. Elsewhere such a label is an imitation (security review 0.22 SPAM-1).
const PREFIX_ENDINGS: &[&str] = &["de", "at"];

/// Words after a brand name in a display name that make an ordinary word the brand ("Apple Support").
const SERVICE_WORDS: &[&str] = &[
    "service",
    "support",
    "kundenservice",
    "kundendienst",
    "team",
    "id",
    "konto",
    "account",
    "security",
    "sicherheit",
    "billing",
    "abrechnung",
    "notification",
    "benachrichtigung",
    "online",
    "banking",
    "info",
    "noreply",
    "no-reply",
];

/// Phrases that ask the reader to log in, confirm data or restore an account (lower case).
const CREDENTIAL_CUES: &[&str] = &[
    "verify your account",
    "confirm your account",
    "verify your identity",
    "confirm your identity",
    "verify your information",
    "update your payment",
    "update your billing",
    "update your account",
    "account has been suspended",
    "account will be suspended",
    "account has been locked",
    "account has been limited",
    "account is on hold",
    "unusual sign-in",
    "unusual activity",
    "suspicious activity",
    "sign in to restore",
    "log in to confirm",
    "login to confirm",
    "password expires",
    "password will expire",
    "mailbox is full",
    "mailbox quota",
    "validate your account",
    "reactivate your account",
    "restore access",
    "konto bestätigen",
    "konto verifizieren",
    "konto zu bestätigen",
    "konto zu verifizieren",
    "identität bestätigen",
    "identität zu bestätigen",
    "daten bestätigen",
    "daten zu bestätigen",
    "daten aktualisieren",
    "daten zu aktualisieren",
    "zahlungsdaten",
    "zahlungsinformationen",
    "konto wurde gesperrt",
    "konto gesperrt",
    "konto wird gesperrt",
    "konto wurde eingeschränkt",
    "zugang wurde gesperrt",
    "ungewöhnliche aktivität",
    "verdächtige aktivität",
    "verdächtige anmeldung",
    "passwort läuft ab",
    "passwort ist abgelaufen",
    "postfach ist voll",
    "speicherplatz ist voll",
    "zugang wiederherstellen",
    "sicherheitsüberprüfung",
    "legitimation",
    "pushtan-verfahren",
    "verifizierung erforderlich",
];

/// What the checks look at.
#[derive(Debug, Clone, Default)]
pub struct Input<'a> {
    /// The From display name, if any.
    pub from_name: Option<&'a str>,
    /// The From address.
    pub from_address: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub subject: &'a str,
    /// The text a reader sees.
    pub text: &'a str,
    pub links: Vec<SeenLink>,
    /// Domains of the reader's contacts and of people they wrote to, for lookalikes of a partner.
    pub contact_domains: &'a [String],
    /// A mailing list rewrites From and Reply-To for good reasons.
    pub mailing_list: bool,
    /// The From domain is authenticated (DMARC passed, or SPF and DKIM without a DMARC policy). Only
    /// then is a link whose text shows the sender's own site a harmless tracking link: anybody can
    /// write a From domain that publishes no DMARC policy.
    pub from_authenticated: bool,
}

/// A link as the checks need it: what it shows and where it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenLink {
    /// The site the link text names, when the text is an address.
    pub named: Option<String>,
    pub target: LinkTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    Host(String),
    Ip(IpAddr),
}

impl SeenLink {
    pub(crate) fn of(link: &Link) -> SeenLink {
        SeenLink {
            named: link.text.as_deref().and_then(links::named_in_text),
            target: match &link.target {
                Target::Domain(domain) => LinkTarget::Host(domain.clone()),
                Target::Ip(ip) => LinkTarget::Ip(*ip),
            },
        }
    }
}

/// Runs every check. Each rule counts once.
pub fn check(input: &Input<'_>) -> Vec<Finding> {
    // Header fields are not length-limited on the way in: cap what the word and lookalike
    // matching sees, whoever calls (security review 0.22 SPAM-4).
    let from_name = input.from_name.map(|name| clip(name, MAX_NAME_CHARS));
    let subject = clip(input.subject, MAX_SUBJECT_CHARS);
    let text = clip(input.text, MAX_TEXT);
    let contact_domains: Vec<&str> = input
        .contact_domains
        .iter()
        .map(String::as_str)
        .filter(|domain| valid_domain(domain))
        .take(MAX_CONTACT_DOMAINS)
        .collect();
    let input = Input { from_name, subject, text, ..input.clone() };
    let input = &input;
    let mut found = Vec::new();
    let from_domain = input.from_address.and_then(domain_of);
    let from_site = from_domain.as_deref().map(site);
    let from_brand = from_site.as_deref().and_then(own_brand);

    if let Some(from_site) = &from_site {
        if from_brand.is_none() {
            if let Some((brand, how)) = imitated_brand(from_site) {
                match how {
                    Imitation::Lookalike => push(
                        &mut found,
                        "LOOKALIKE_BRAND_FROM",
                        4.0,
                        format!("{from_site} looks like {}", brand.names[0]),
                    ),
                    Imitation::Contains => push(
                        &mut found,
                        "BRAND_IN_FROM_DOMAIN",
                        2.0,
                        format!("{from_site} carries the name {}", brand.names[0]),
                    ),
                }
            }
            if let Some(contact) = imitated_contact(from_site, &contact_domains) {
                push(&mut found, "LOOKALIKE_CONTACT_FROM", 4.0, format!("{from_site} looks like {contact}"));
            }
        }
        if let Some(name) = input.from_name {
            if let Some(shown) = address_in_name(name, from_site) {
                push(&mut found, "FROM_NAME_SPOOFS_ADDRESS", 3.0, format!("{shown} shown, sent from {from_site}"));
            } else if !input.mailing_list
                && let Some(shown) = domain_in_name(name, from_site)
            {
                push(&mut found, "FROM_NAME_SHOWS_DOMAIN", 2.0, format!("{shown} shown, sent from {from_site}"));
            }
            if !input.mailing_list
                && let Some(brand) = brand_in_name(name)
                && from_brand.is_none_or(|own| !std::ptr::eq(own, brand))
            {
                push(&mut found, "BRAND_IN_FROM_NAME", 2.5, format!("\"{}\" sent from {from_site}", one_line(name)));
            }
        }
        if !input.mailing_list
            && let Some(reply_to) = input.reply_to.and_then(domain_of)
            && site(&reply_to) != *from_site
            && !same_brand(&site(&reply_to), from_site)
        {
            push(&mut found, "REPLY_TO_OTHER_SITE", 0.5, format!("answers go to {}", site(&reply_to)));
        }
    }

    for link in &input.links {
        let LinkTarget::Host(host) = &link.target else {
            if let Some(named) = &link.named {
                push(&mut found, "PHISHING_LINK_TEXT", 3.0, format!("{named} -> {}", target_text(&link.target)));
            }
            continue;
        };
        if !valid_domain(host) {
            continue;
        }
        let target_site = site(host);
        if own_brand(&target_site).is_none()
            && from_site.as_deref() != Some(target_site.as_str())
            && let Some((brand, Imitation::Lookalike)) = imitated_brand(&target_site)
        {
            push(&mut found, "LOOKALIKE_BRAND_LINK", 3.0, format!("{target_site} looks like {}", brand.names[0]));
        }
        if let Some(named) = link.named.as_deref().filter(|named| valid_domain(named)) {
            let named_site = site(named);
            // A link whose text is a brand's address and leads elsewhere is the classic trick, whoever
            // sent it. Its text naming the sender's own site is what every tracking link of a
            // newsletter does, and says little by itself — when the sender is who it claims to be
            // (security review 0.22 SPAM-3).
            if named_site != target_site && !same_brand(&named_site, &target_site) {
                if own_brand(&named_site).is_some() || contact_domains.iter().any(|d| site(d) == named_site) {
                    push(&mut found, "BRAND_LINK_TEXT", 3.0, format!("{named} -> {target_site}"));
                } else if input.from_authenticated && from_site.as_deref() == Some(named_site.as_str()) {
                    push(&mut found, "TRACKED_LINK_TEXT", 0.0, format!("{named} -> {target_site}"));
                } else {
                    push(&mut found, "PHISHING_LINK_TEXT", 3.0, format!("{named} -> {target_site}"));
                }
            }
        }
    }

    let lower_subject = input.subject.to_lowercase();
    if from_brand.is_none()
        && !input.mailing_list
        && let Some(brand) = brand_in_text(&lower_subject)
        && let Some(cue) = credential_cue(&format!("{lower_subject}\n{}", input.text.to_lowercase()))
    {
        push(&mut found, "BRAND_IN_SUBJECT", 1.5, format!("{} with \"{cue}\"", brand.names[0]));
    }

    // Asking for a login or data is normal for a service's own mail; asking for it with links to
    // somewhere else is how credentials are phished.
    // A mail that claims to be a brand it does not come from has no site of its own to send
    // anybody to: every link that is not the brand's counts as somewhere else.
    if let Some(cue) = credential_cue(&format!("{lower_subject}\n{}", input.text.to_lowercase())) {
        let claims_brand = found.iter().any(|finding| {
            matches!(
                finding.rule,
                "BRAND_IN_FROM_NAME" | "LOOKALIKE_BRAND_FROM" | "BRAND_IN_FROM_DOMAIN" | "BRAND_IN_SUBJECT"
            )
        });
        let own = |link: &SeenLink| match &link.target {
            LinkTarget::Ip(_) => false,
            LinkTarget::Host(host) => {
                let target = site(host);
                if claims_brand {
                    own_brand(&target).is_some()
                } else {
                    from_site.as_deref() == Some(target.as_str())
                        || from_site.as_deref().is_some_and(|from| same_brand(from, &target))
                }
            }
        };
        // A newsletter that mentions updating data links to its own site somewhere, if only in the
        // footer; a phishing mail does not. A false brand has no own site at all.
        let elsewhere = if claims_brand || !input.links.iter().any(own) {
            input.links.iter().find(|link| !own(link)).map(|link| match &link.target {
                LinkTarget::Ip(ip) => ip.to_string(),
                LinkTarget::Host(host) => site(host),
            })
        } else {
            None
        };
        if let Some(target) = elsewhere {
            push(&mut found, "CREDENTIAL_REQUEST", 2.0, format!("\"{cue}\", link to {target}"));
        }
    }
    found
}

/// Runs the checks on a stored message. `contact_domains` are the reader's partners' domains.
/// `from_authenticated` as in [`Input`].
pub fn check_message(raw: &[u8], contact_domains: &[String], from_authenticated: bool) -> Vec<Finding> {
    let Some(message) = parse_message(&raw[..raw.len().min(super::content::MAX_MESSAGE)]) else { return Vec::new() };
    let read = read_message(&message);
    let input = Input { contact_domains, from_authenticated, ..read.input() };
    check(&input)
}

/// The links written out in a plain text, for mail that only comes as text (another account's).
pub fn links_in_text(text: &str) -> Vec<SeenLink> {
    links::in_text(text).iter().take(200).map(SeenLink::of).collect()
}

/// What [`check`] needs out of a parsed message, owned.
#[derive(Debug, Default)]
pub(crate) struct Read {
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub text: String,
    pub links: Vec<SeenLink>,
    pub mailing_list: bool,
}

impl Read {
    pub(crate) fn input(&self) -> Input<'_> {
        Input {
            from_name: self.from_name.as_deref(),
            from_address: self.from_address.as_deref(),
            reply_to: self.reply_to.as_deref(),
            subject: &self.subject,
            text: &self.text,
            links: self.links.clone(),
            contact_domains: &[],
            mailing_list: self.mailing_list,
            from_authenticated: false,
        }
    }
}

/// The text is read up to this many characters for the cues.
const MAX_TEXT: usize = 64 * 1024;

pub(crate) fn read_message(message: &Message<'_>) -> Read {
    let from = message.from().and_then(|from| from.first());
    let mut found = Vec::new();
    let mut text = String::new();
    let mut ids: Vec<u32> = message.text_body.iter().chain(&message.html_body).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    for part in ids.iter().filter_map(|id| message.parts.get(*id as usize)) {
        match &part.body {
            PartType::Text(body) => {
                found.extend(links::in_text(body));
                if text.len() < MAX_TEXT {
                    text.push_str(body);
                    text.push('\n');
                }
            }
            PartType::Html(body) => found.extend(links::in_anchors(&html::read(body).anchors)),
            _ => {}
        }
    }
    if text.trim().is_empty()
        && let Some(body) = message.body_text(0)
    {
        text.push_str(&body);
    }
    let mut cut = text.len().min(MAX_TEXT);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    Read {
        from_name: from.and_then(|from| from.name.as_deref()).map(str::to_owned),
        from_address: from.and_then(|from| from.address.as_deref()).map(str::to_owned),
        reply_to: message
            .reply_to()
            .and_then(|reply| reply.first())
            .and_then(|reply| reply.address.as_deref())
            .map(str::to_owned),
        subject: message.subject().unwrap_or_default().chars().take(1000).collect(),
        text,
        links: found.iter().take(200).map(SeenLink::of).collect(),
        mailing_list: message.header("List-Id").is_some() || message.header("List-Post").is_some(),
    }
}

fn push(found: &mut Vec<Finding>, rule: &'static str, points: f32, detail: String) {
    if !found.iter().any(|finding| finding.rule == rule) {
        found.push(Finding { rule, points, detail });
    }
}

fn target_text(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Host(host) => host.clone(),
        LinkTarget::Ip(ip) => ip.to_string(),
    }
}

fn one_line(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(120).collect::<String>().trim().to_owned()
}

/// The domain of an address, lower case, without a trailing dot.
pub fn domain_of(address: &str) -> Option<String> {
    let (_, domain) = address.trim().trim_matches(['<', '>']).rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('.');
    if !valid_domain(domain) {
        return None;
    }
    let domain = domain.to_ascii_lowercase();
    domain.contains('.').then_some(domain)
}

/// The longest From display name the checks read, in characters.
const MAX_NAME_CHARS: usize = 256;
/// The longest subject the checks read, in characters.
const MAX_SUBJECT_CHARS: usize = 1000;
/// At most this many contact domains are compared with the sender's.
pub const MAX_CONTACT_DOMAINS: usize = 2000;

/// Whether a name can be a DNS domain at all: at most 253 bytes, labels of 1 to 63. Anything
/// longer is no real host and only costs time in the lookalike matching.
fn valid_domain(domain: &str) -> bool {
    let domain = domain.strip_suffix('.').unwrap_or(domain);
    !domain.is_empty() && domain.len() <= 253 && domain.split('.').all(|label| !label.is_empty() && label.len() <= 63)
}

/// The first `max` bytes' worth of a text, cut at a character boundary.
fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

/// The first label of a site and its ending: `("paypal", "co.uk")`.
fn label_and_ending(site: &str) -> (&str, &str) {
    site.split_once('.').unwrap_or((site, ""))
}

/// The brand a site belongs to, if it is one of a brand's own.
pub fn own_brand(site: &str) -> Option<&'static Brand> {
    own_brand_by(site).map(|(brand, _)| brand)
}

/// The brand a site belongs to, and whether it only belongs to it through the prefix rule of a
/// regional brand (`sparkasse-musterstadt.de`), which is weaker evidence than an exact match.
fn own_brand_by(site: &str) -> Option<(&'static Brand, bool)> {
    let (label, ending) = label_and_ending(site);
    BRANDS.iter().find_map(|brand| {
        if brand.sites.contains(&site) || (COMMON_ENDINGS.contains(&ending) && brand.labels.contains(&label)) {
            return Some((brand, false));
        }
        let prefixed = brand.own_prefix
            && PREFIX_ENDINGS.contains(&ending)
            && brand
                .labels
                .iter()
                .any(|own| label.strip_prefix(own).is_some_and(|rest| rest.len() > 1 && rest.starts_with('-')));
        prefixed.then_some((brand, true))
    })
}

/// Whether two sites are the same brand's own. A site that only counts as the brand's through the
/// prefix rule never vouches for another one: `sparkasse.de` shown on a link to
/// `sparkasse-musterstadt.de` is still a mismatch worth a hint.
fn same_brand(a: &str, b: &str) -> bool {
    matches!(
        (own_brand_by(a), own_brand_by(b)),
        (Some((x, false)), Some((y, false))) if std::ptr::eq(x, y)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Imitation {
    /// Spelled to look like the brand: `paypa1`, `rnicrosoft`, a Cyrillic `а`, one letter off.
    Lookalike,
    /// The brand's name with something around it: `paypal-sicherheit`, `secure-dhl-paket`.
    Contains,
}

/// Which brand a site that is not the brand's own imitates, if any.
fn imitated_brand(site: &str) -> Option<(&'static Brand, Imitation)> {
    let (label, ending) = label_and_ending(site);
    let unicode = unicode_label(label);
    let seen = skeleton(&unicode);
    let mut contains = None;
    for brand in BRANDS {
        for own in brand.labels {
            let own_skeleton = skeleton(own);
            // The brand's own name under an unusual ending, or spelled to look like it.
            if (label == *own && !COMMON_ENDINGS.contains(&ending))
                || (label != *own && seen == own_skeleton && own.len() >= 3)
                || (own.len() >= 6 && seen.len() >= 5 && edit_distance_one(&seen, &own_skeleton))
            {
                return Some((brand, Imitation::Lookalike));
            }
            // One part of a hyphenated name spelled like the brand: `netfllx-billing`.
            if seen.contains('-')
                && unicode.split('-').zip(seen.split('-')).any(|(written, part)| {
                    (written != *own && part == own_skeleton && own.len() >= 4)
                        || (own.len() >= 6 && part.len() >= 5 && edit_distance_one(part, &own_skeleton))
                })
            {
                return Some((brand, Imitation::Lookalike));
            }
            if contains.is_none() && carries(&seen, &own_skeleton, brand) {
                contains = Some(brand);
            }
        }
    }
    contains.map(|brand| (brand, Imitation::Contains))
}

/// Whether a label carries a brand's name with something around it.
fn carries(skeleton: &str, own: &str, brand: &Brand) -> bool {
    if skeleton == own || own.len() < 3 {
        return false;
    }
    // Short names only as a whole part between hyphens; longer ones anywhere.
    let parts: Vec<&str> = skeleton.split('-').collect();
    if parts.len() > 1 && parts.contains(&own) && (brand.distinct || own.len() >= 4) {
        return true;
    }
    own.len() >= 5 && brand.distinct && skeleton.replace('-', "").contains(own)
}

/// Which contact's domain a site imitates: one letter off, or spelled to look like it, but not the
/// same name under another ending (which is usually the same company).
fn imitated_contact(site: &str, contacts: &[&str]) -> Option<String> {
    let (label, _) = label_and_ending(site);
    let seen = skeleton(&unicode_label(label));
    contacts.iter().map(|domain| self::site(domain)).find(|contact| {
        let (theirs, _) = label_and_ending(contact);
        if contact == site || theirs == label || own_brand(contact).is_some() {
            return false;
        }
        let their_skeleton = skeleton(theirs);
        (seen == their_skeleton && theirs.len() >= 3)
            || (theirs.len() >= 5 && edit_distance_one(&seen, &their_skeleton))
    })
}

/// A punycode label as the reader sees it.
fn unicode_label(label: &str) -> String {
    if !label.starts_with("xn--") {
        return label.to_owned();
    }
    let (unicode, result) = idna::domain_to_unicode(label);
    if result.is_err() { label.to_owned() } else { unicode }
}

/// What a label looks like to a quick reader: lookalike letters and digits mapped to the Latin
/// letters they imitate, `rn` as `m` and `vv` as `w`. Hyphens stay.
fn skeleton(label: &str) -> String {
    let mapped: String = label.to_lowercase().chars().map(confusable).collect();
    mapped.replace("rn", "m").replace("vv", "w")
}

fn confusable(c: char) -> char {
    match c {
        '0' | 'о' | 'ο' | 'օ' => 'o',
        '1' | 'l' | 'ӏ' | 'ı' | 'і' | 'ι' | '|' => 'l',
        '3' | 'е' | 'ε' => 'e',
        '4' | 'а' | 'α' => 'a',
        '5' | 'ѕ' => 's',
        '7' => 't',
        'р' | 'ρ' => 'p',
        'с' | 'ϲ' => 'c',
        'у' | 'γ' => 'y',
        'х' | 'χ' => 'x',
        'ј' => 'j',
        'ԁ' => 'd',
        'һ' => 'h',
        'ԛ' => 'q',
        'ԝ' => 'w',
        'ν' => 'v',
        'κ' => 'k',
        'ь' => 'b',
        'υ' => 'u',
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        other => other,
    }
}

/// Whether two names are one edit apart: a letter changed, added, dropped or two swapped.
fn edit_distance_one(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b {
        return false;
    }
    match a.len() as isize - b.len() as isize {
        0 => {
            let diff: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
            diff.len() == 1
                || (diff.len() == 2 && diff[1] == diff[0] + 1 && a[diff[0]] == b[diff[1]] && a[diff[1]] == b[diff[0]])
        }
        1 => one_dropped(&a, &b),
        -1 => one_dropped(&b, &a),
        _ => false,
    }
}

/// Whether `shorter` is `longer` with one letter left out.
fn one_dropped(longer: &[char], shorter: &[char]) -> bool {
    let first = longer.iter().zip(shorter).take_while(|(x, y)| x == y).count();
    longer[first + 1..] == shorter[first..]
}

/// An address in a display name of another site than the one the mail comes from, like
/// "service@bank.example" <someone@elsewhere.example>.
fn address_in_name(name: &str, from_site: &str) -> Option<String> {
    name.split(|c: char| c.is_whitespace() || "<>()[]\"',;:".contains(c))
        .filter_map(|word| word.rsplit_once('@').map(|(_, domain)| domain.trim_end_matches('.').to_ascii_lowercase()))
        .find(|domain| domain.contains('.') && site(domain) != from_site && !same_brand(&site(domain), from_site))
}

/// A domain name written in a display name, of another site than the one the mail comes from, like
/// "uwu.example Postmaster" <postmaster@elsewhere.example>.
fn domain_in_name(name: &str, from_site: &str) -> Option<String> {
    name.split(|c: char| c.is_whitespace() || "<>()[]\"',;:".contains(c))
        .filter(|word| !word.contains('@'))
        .filter_map(links::named_in_text)
        .find(|domain| site(domain) != from_site && !same_brand(&site(domain), from_site))
}

/// The words of a text, lower case, split at anything that is not a letter, digit, `&`, `+` or a
/// dot inside a word (`booking.com`).
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '&' || c == '+' || c == '.'))
        .map(|word| word.trim_matches('.').to_owned())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `parts` (a name's words) stand in `words` at `at`.
fn name_at(words: &[String], at: usize, parts: &[String]) -> Option<usize> {
    (!parts.is_empty() && words.len() >= at + parts.len() && words[at..at + parts.len()] == parts[..])
        .then_some(parts.len())
}

/// Each brand's names as words, split once ([`BRANDS`] order).
fn brand_words() -> &'static [Vec<Vec<String>>] {
    static WORDS: std::sync::OnceLock<Vec<Vec<Vec<String>>>> = std::sync::OnceLock::new();
    WORDS.get_or_init(|| BRANDS.iter().map(|brand| brand.names.iter().map(|name| words(name)).collect()).collect())
}

/// The brand a display name claims to be.
fn brand_in_name(name: &str) -> Option<&'static Brand> {
    let words = words(name);
    if words.is_empty() {
        return None;
    }
    BRANDS.iter().zip(brand_words()).find_map(|(brand, names)| {
        names
            .iter()
            .any(|parts| {
                (0..words.len()).any(|at| {
                    let Some(len) = name_at(&words, at, parts) else { return false };
                    if brand.distinct {
                        return true;
                    }
                    // An ordinary word only as the whole name, or with a service word around it.
                    at == 0 && words[len..].iter().all(|word| SERVICE_WORDS.contains(&word.as_str()))
                })
            })
            .then_some(brand)
    })
}

/// A distinctive brand named in a (lower case) text.
fn brand_in_text(text: &str) -> Option<&'static Brand> {
    let words = words(text);
    BRANDS.iter().zip(brand_words()).filter(|(brand, _)| brand.distinct).find_map(|(brand, names)| {
        names.iter().any(|parts| (0..words.len()).any(|at| name_at(&words, at, parts).is_some())).then_some(brand)
    })
}

/// The first phrase in a (lower case) text that asks for a login or data.
fn credential_cue(text: &str) -> Option<&'static str> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    CREDENTIAL_CUES.iter().copied().find(|cue| text.contains(cue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(input: &Input<'_>) -> Vec<&'static str> {
        let mut rules: Vec<_> = check(input).into_iter().map(|finding| finding.rule).collect();
        rules.sort_unstable();
        rules
    }

    fn link(named: Option<&str>, host: &str) -> SeenLink {
        SeenLink { named: named.map(str::to_owned), target: LinkTarget::Host(host.to_owned()) }
    }

    /// One of a brand's own domains, without writing a real domain into a test.
    fn own(name: &str) -> String {
        let brand = BRANDS.iter().find(|brand| brand.names[0] == name).unwrap();
        format!("{}.com", brand.labels[0])
    }

    #[test]
    fn lookalikes_of_brands_are_seen_through() {
        for domain in [
            "paypa1.example",
            "rnicrosoft.example",
            "paypall.example",
            "amaz0n.example",
            "netfllx.example",
            "netfllx-billing.example",
            "micros0ft-365.example",
        ] {
            let address = format!("service@{domain}");
            let input = Input { from_address: Some(&address), ..Input::default() };
            assert_eq!(rules(&input), ["LOOKALIKE_BRAND_FROM"], "{domain}");
        }
        let cyrillic = idna::domain_to_ascii("p\u{430}ypal.example").unwrap();
        let address = format!("service@{cyrillic}");
        assert_eq!(rules(&Input { from_address: Some(&address), ..Input::default() }), ["LOOKALIKE_BRAND_FROM"]);

        // The brand's name under an ending no brand uses, and the name with something around it.
        let input = Input { from_address: Some("noreply@paypal.example"), ..Input::default() };
        assert_eq!(rules(&input), ["LOOKALIKE_BRAND_FROM"]);
        for domain in ["paypal-sicherheit.example", "secure-dhl-paket.example", "sparkasse-online.example"] {
            let address = format!("info@{domain}");
            assert_eq!(rules(&Input { from_address: Some(&address), ..Input::default() }), ["BRAND_IN_FROM_DOMAIN"]);
        }
    }

    #[test]
    fn ordinary_domains_are_left_alone() {
        for domain in ["shop.example", "apply.example", "ups-and-downs.example", "dhlx.example", "posteo.example"] {
            let address = format!("info@{domain}");
            assert!(rules(&Input { from_address: Some(&address), ..Input::default() }).is_empty(), "{domain}");
        }
        // A brand's own domain is its own.
        let paypal = format!("service@{}", own("PayPal"));
        let input = Input { from_address: Some(&paypal), from_name: Some("PayPal"), ..Input::default() };
        assert!(rules(&input).is_empty());
        let sparkasse = "info@sparkasse-musterstadt.de";
        assert!(own_brand(&site(&domain_of(sparkasse).unwrap())).is_some(), "regional banks have many domains");
    }

    /// Security review 0.22 SPAM-1: the prefix rule of regional banks only holds under their own
    /// endings; `sparkasse-<anything>` elsewhere is what phishing registers.
    #[test]
    fn regional_prefixes_only_count_under_their_endings() {
        for domain in ["sparkasse-sicherheit.net", "sparkasse-login.com", "volksbank-hilfe.eu"] {
            assert!(own_brand(domain).is_none(), "{domain}");
            let address = format!("info@{domain}");
            assert_eq!(
                rules(&Input { from_address: Some(&address), ..Input::default() }),
                ["BRAND_IN_FROM_DOMAIN"],
                "{domain}"
            );
        }
        assert!(own_brand("sparkasse-musterstadt.at").is_some());
        assert!(own_brand("sparkasse-.de").is_none(), "a bare hyphen is no regional name");
        // The brand's address shown on a link to a prefix domain elsewhere is the classic trick.
        let input = Input {
            from_address: Some("info@sparkasse-login.com"),
            links: vec![link(Some("www.sparkasse.de"), "sparkasse-login.com")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_IN_FROM_DOMAIN", "BRAND_LINK_TEXT"]);
        // Even under .de a prefix domain does not vouch for a link that shows the brand's own site.
        let input = Input {
            from_address: Some("info@sparkasse-musterstadt.de"),
            links: vec![link(Some("www.sparkasse.de"), "sparkasse-musterstadt.de")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_LINK_TEXT"]);
        // A credential request from a fake prefix domain is one.
        let input = Input {
            from_address: Some("info@sparkasse-sicherheit.net"),
            text: "Bitte Konto verifizieren.",
            links: vec![link(None, "sparkasse-sicherheit.net")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_IN_FROM_DOMAIN", "CREDENTIAL_REQUEST"]);
    }

    #[test]
    fn display_names_that_claim_a_brand_or_an_address() {
        let input = Input {
            from_name: Some("PayPal Service"),
            from_address: Some("a@konto-hilfe.example"),
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_IN_FROM_NAME"]);
        let input = Input {
            from_name: Some("service@bank.example"),
            from_address: Some("alert@evil.example"),
            ..Input::default()
        };
        assert_eq!(rules(&input), ["FROM_NAME_SPOOFS_ADDRESS"]);
        let input = Input {
            from_name: Some("uwu.example Postmaster"),
            from_address: Some("postmaster@mail-upgrade.example"),
            ..Input::default()
        };
        assert_eq!(rules(&input), ["FROM_NAME_SHOWS_DOMAIN"]);
        let input =
            Input { from_name: Some("Shop.example Team"), from_address: Some("news@shop.example"), ..Input::default() };
        assert!(rules(&input).is_empty(), "its own domain is fine");
        // Ordinary words are no brand unless they are the whole name.
        for name in ["Apple Support", "Steam", "ING Kundenservice"] {
            let input = Input { from_name: Some(name), from_address: Some("a@x.example"), ..Input::default() };
            assert_eq!(rules(&input), ["BRAND_IN_FROM_NAME"], "{name}");
        }
        for name in
            ["Apfel & Apple Hofladen", "Steam Punk Festival", "Visa Service Reisebüro Müller", "Booking Team Lisa"]
        {
            let input = Input { from_name: Some(name), from_address: Some("a@x.example"), ..Input::default() };
            assert!(rules(&input).is_empty(), "{name}");
        }
        // A mailing list may put anybody's name on its own address.
        let input = Input {
            from_name: Some("PayPal Fan via Liste"),
            from_address: Some("liste@lists.example"),
            mailing_list: true,
            ..Input::default()
        };
        assert!(rules(&input).is_empty());
    }

    #[test]
    fn lookalikes_of_a_partner() {
        let contacts = vec!["firma.example".to_owned(), "kanzlei-weber.example".to_owned()];
        let input = Input { from_address: Some("chef@flrma.example"), contact_domains: &contacts, ..Input::default() };
        assert_eq!(rules(&input), ["LOOKALIKE_CONTACT_FROM"]);
        let input = Input {
            from_address: Some("buero@kanzlei-webber.example"),
            contact_domains: &contacts,
            ..Input::default()
        };
        assert_eq!(rules(&input), ["LOOKALIKE_CONTACT_FROM"]);
        // The partner itself, another ending of the same name, or something unrelated.
        for from in ["chef@firma.example", "chef@firma.test", "info@farm.example"] {
            let input = Input { from_address: Some(from), contact_domains: &contacts, ..Input::default() };
            assert!(rules(&input).is_empty(), "{from}");
        }
    }

    #[test]
    fn link_texts_that_name_another_site() {
        let paypal = own("PayPal");
        let input = Input {
            from_address: Some("news@shop.example"),
            links: vec![link(Some(&paypal), "login.evil.example")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_LINK_TEXT"]);
        let input = Input {
            from_address: Some("news@shop.example"),
            links: vec![link(Some("www.bank.example"), "evil.example")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["PHISHING_LINK_TEXT"]);
        // A newsletter's tracking link that shows the shop's own address is no trick.
        let input = Input {
            from_address: Some("news@shop.example"),
            links: vec![link(Some("shop.example"), "click.mailer.example")],
            from_authenticated: true,
            ..Input::default()
        };
        assert_eq!(rules(&input), ["TRACKED_LINK_TEXT"]);
        assert_eq!(check(&input)[0].points, 0.0);
        // Unless the From domain is not authenticated: anybody can write it (SPAM-3).
        let input = Input { from_authenticated: false, ..input };
        assert_eq!(rules(&input), ["PHISHING_LINK_TEXT"]);
        assert_eq!(check(&input)[0].points, 3.0);
        // A link to a lookalike, and one to an IP address under an address text.
        let input = Input {
            from_address: Some("a@x.example"),
            links: vec![
                link(None, "www.paypa1.example"),
                SeenLink { named: Some("bank.example".into()), target: LinkTarget::Ip("192.0.2.7".parse().unwrap()) },
            ],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["LOOKALIKE_BRAND_LINK", "PHISHING_LINK_TEXT"]);
    }

    #[test]
    fn asking_for_a_login_with_links_elsewhere() {
        let text = "Ihr Konto wurde gesperrt. Bitte bestätigen Sie Ihre Daten.";
        let input = Input {
            from_address: Some("service@kundenportal.example"),
            subject: "Wichtig: Ihr PayPal-Konto",
            text,
            links: vec![link(None, "konto-check.example")],
            ..Input::default()
        };
        assert_eq!(rules(&input), ["BRAND_IN_SUBJECT", "CREDENTIAL_REQUEST"]);
        // A service's own password mail links to the service.
        let input = Input {
            from_address: Some("noreply@service.example"),
            subject: "Unusual sign-in to your account",
            text: "We noticed an unusual sign-in. If this was you, ignore this mail.",
            links: vec![link(None, "account.service.example")],
            ..Input::default()
        };
        assert!(rules(&input).is_empty());
    }

    #[test]
    fn replies_to_another_site() {
        let input = Input {
            from_address: Some("ceo@firma.example"),
            reply_to: Some("ceo.firma@mail.example"),
            ..Input::default()
        };
        assert_eq!(rules(&input), ["REPLY_TO_OTHER_SITE"]);
        let input = Input {
            from_address: Some("news@a.shop.example"),
            reply_to: Some("help@b.shop.example"),
            ..Input::default()
        };
        assert!(rules(&input).is_empty());
    }

    #[test]
    fn edits_are_counted_once() {
        assert!(edit_distance_one("paypal", "paypall"));
        assert!(edit_distance_one("paypal", "papyal"));
        assert!(edit_distance_one("paypal", "paypa"));
        assert!(edit_distance_one("paypal", "pqypal"));
        assert!(!edit_distance_one("paypal", "paypal"));
        assert!(!edit_distance_one("paypal", "pyapla"));
        assert!(!edit_distance_one("amazon", "amazing"));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let long = "ä".repeat(10_000);
        let address = format!("{long}@{long}.example");
        let input = Input {
            from_name: Some(&long),
            from_address: Some(&address),
            reply_to: Some("@"),
            subject: &long,
            text: &long,
            links: vec![link(Some(&long), &long), link(Some("xn--"), "xn--.example")],
            ..Input::default()
        };
        let _ = check(&input);
        let _ = check_message(b"\xff\xfe garbage", &[], false);
    }

    /// Security review 0.22 SPAM-4: a megabyte display name, subject or host costs next to nothing.
    /// The limit is generous so a loaded machine still passes; uncapped this took seconds.
    #[test]
    fn huge_header_fields_are_cheap() {
        let name = "PayPal Apple Steam Service ".repeat(40_000);
        let label = "netfllx-".repeat(250_000);
        let host = format!("{label}.example");
        let long_address = format!("a@{host}");
        let contacts: Vec<String> = (0..5000).map(|i| format!("firma{i}-{}.example", "x".repeat(50))).collect();
        let links: Vec<SeenLink> = (0..200).map(|_| link(Some(&host), &host)).collect();
        let started = std::time::Instant::now();
        let input = Input {
            from_name: Some(&name),
            from_address: Some("a@paypa1-konto.example"),
            reply_to: Some(&long_address),
            subject: &name,
            text: &name,
            links,
            contact_domains: &contacts,
            ..Input::default()
        };
        let found = check(&input);
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
        // The capped name still names the brand; the overlong host is no domain at all.
        assert!(found.iter().any(|finding| finding.rule == "BRAND_IN_FROM_NAME"), "{found:?}");
        assert!(domain_of(&long_address).is_none());
        assert!(!valid_domain(&format!("{}.example", "a".repeat(64))));
        assert!(valid_domain(&format!("{}.example", "a".repeat(63))));
    }
}
