//! Facts about a mail that need no model (docs/labels.md, "Facts"): how it was sent (to a list, in
//! bulk, automatically), who sent it (a person, a no-reply address, a marketing sender), and what it
//! contains (amounts, invoice numbers, tracking numbers, dates, codes, sales words). The detectors
//! decide by them, and the model gets them written out, so it confirms what is there instead of
//! guessing.

use serde::Serialize;

use crate::Mail;
use crate::text::{find_any, fold, has_word, words};

/// Of each list of found things, at most this many are kept.
const MAX_FOUND: usize = 3;

/// What kind of address sent the mail, by the address and its display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SenderKind {
    /// `noreply@`, `notifications@`, `alerts@`, … : a system that reads no answers.
    NoReply,
    /// `news@`, `newsletter@`, `marketing@`, or a marketing subdomain (`email.shop.example`).
    Marketing,
    /// `info@`, `service@`, `support@`, `billing@`, … : a company's shared address.
    Role,
    /// Looks like one person's address and name.
    Person,
    /// None of these for sure.
    Unknown,
}

impl SenderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SenderKind::NoReply => "noReply",
            SenderKind::Marketing => "marketing",
            SenderKind::Role => "role",
            SenderKind::Person => "person",
            SenderKind::Unknown => "unknown",
        }
    }
}

/// Everything known about a mail without a model.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    pub list_unsubscribe: bool,
    /// `List-Id` or `Precedence: bulk/list`, or `List-Unsubscribe-Post` (one-click unsubscribe).
    pub bulk: bool,
    /// `List-Post`: a discussion list people write to.
    pub discussion_list: bool,
    /// `Auto-Submitted` other than `no`, or `X-Auto-Response-Suppress`.
    pub automatic: bool,
    pub sender: SenderKind,
    /// A bounce or delivery report from a mail server: none of the base labels.
    pub bounce: bool,
    /// The sender's domain is a freemail provider's.
    pub freemail: bool,
    /// The sender's domain is that of a recipient, and no freemail provider's: a colleague.
    pub same_domain: bool,
    /// SPF, DKIM or DMARC vouch for the From address.
    pub authenticated: bool,
    pub known_sender: bool,
    pub amounts: Vec<String>,
    pub invoice_numbers: Vec<String>,
    /// Carrier and tracking number, when a number names its carrier or a carrier is named beside one.
    pub tracking: Option<(String, String)>,
    /// The first carrier named (DHL, DPD, Hermes, GLS, UPS, Amazon).
    pub carrier: Option<String>,
    pub dates: Vec<String>,
    pub times: Vec<String>,
    pub calendar: bool,
    pub pdf: bool,
    /// A one-time code right after "code", "PIN" or "TAN".
    pub code: Option<String>,
    /// Sales words found (discounts, coupons, "today only", review requests …).
    pub sales: Vec<String>,
    /// Account words in the subject (password, sign-in, verify, terms …).
    pub account: Vec<String>,
    /// A greeting or farewell as friends write them ("Hi", "LG", "Cheers").
    pub casual: bool,
    /// A greeting or farewell as business letters have them ("Sehr geehrte", "Kind regards").
    pub formal: bool,
    /// An order word in the subject ("Bestellung", "your order", "Bestellt:").
    pub order: bool,
    /// An invoice word in the subject, or a PDF named like an invoice.
    pub invoice_word: bool,
    /// Sent to the sender's own address (a note to oneself, a test).
    pub to_self: bool,
    /// A notification of an app or social network about activity on the reader's account: new
    /// followers, posts of people they follow, mentions, recaps ("recap@", "du hast neue …").
    pub notification: bool,
    /// A test mail: the subject is only "Test", "Testmail" or the like.
    pub test_mail: bool,
    /// Something says it is a shipment: a tracking number, a carrier, a shipping or pickup word.
    pub shipment_evidence: bool,
}

/// Local parts of addresses that read no answers.
const NO_REPLY: &[&str] = &[
    "noreply",
    "no-reply",
    "no_reply",
    "donotreply",
    "do-not-reply",
    "do_not_reply",
    "notification",
    "notifications",
    "notify",
    "alert",
    "alerts",
    "mailer-daemon",
    "postmaster",
    "automated",
    "system",
    "bounce",
    "bounces",
    "server",
    "root",
    "cron",
    "daemon",
    "monitoring",
    "backup",
    "keineantwort",
    "nicht-antworten",
];
const MARKETING: &[&str] = &[
    "news",
    "newsletter",
    "newsletters",
    "marketing",
    "promo",
    "promotions",
    "angebote",
    "offers",
    "deals",
    "aktion",
    "mailing",
];
/// First labels of a sender's domain that marketing mail is sent from.
const MARKETING_SUBDOMAINS: &[&str] = &[
    "news",
    "newsletter",
    "email",
    "em",
    "e",
    "mailing",
    "marketing",
    "promo",
    "mkt",
    "campaign",
    "crm",
    "info",
    "mailer",
];
const ROLE: &[&str] = &[
    "info",
    "service",
    "support",
    "kontakt",
    "contact",
    "hello",
    "hallo",
    "hi",
    "team",
    "billing",
    "rechnung",
    "rechnungen",
    "invoice",
    "invoices",
    "buchhaltung",
    "accounting",
    "order",
    "orders",
    "bestellung",
    "bestellungen",
    "shop",
    "store",
    "sales",
    "vertrieb",
    "kundenservice",
    "customerservice",
    "customer-service",
    "help",
    "hilfe",
    "accounts",
    "account",
    "security",
    "sicherheit",
    "admin",
    "office",
    "buero",
    "mail",
    "webmaster",
    "jobs",
    "careers",
    "karriere",
    "versand",
    "shipping",
    "shipment-tracking",
    "tracking",
    "delivery",
    "receipts",
    "payments",
    "zahlungen",
    "verwaltung",
    "praxis",
    "termine",
    "booking",
    "reservations",
    "events",
    "community",
    "feedback",
    "survey",
    "umfrage",
    "privacy",
    "legal",
    "hr",
    "personal",
    "bewerbung",
];
/// Words in a display name that say a company or a system, not a person.
const COMPANY_WORDS: &[&str] = &[
    "gmbh",
    "ag",
    "ug",
    "kg",
    "e.v.",
    "ev",
    "inc",
    "ltd",
    "llc",
    "corp",
    "corporation",
    "team",
    "support",
    "service",
    "kundenservice",
    "shop",
    "store",
    "newsletter",
    "news",
    "info",
    "noreply",
    "no-reply",
    "konto",
    "account",
    "bank",
    "versicherung",
    "verlag",
    "stadtwerke",
    "praxis",
    "jobs",
    "billing",
    "security",
    "notifications",
    "community",
    "system",
    "online",
    "de",
    "com",
];
/// Freemail providers: an address there is a person's, never a company's colleague.
pub const FREEMAIL: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "gmx.de",
    "gmx.net",
    "gmx.at",
    "gmx.ch",
    "web.de",
    "t-online.de",
    "freenet.de",
    "arcor.de",
    "mail.de",
    "posteo.de",
    "posteo.net",
    "mailbox.org",
    "proton.me",
    "protonmail.com",
    "pm.me",
    "outlook.com",
    "outlook.de",
    "hotmail.com",
    "hotmail.de",
    "live.com",
    "live.de",
    "msn.com",
    "yahoo.com",
    "yahoo.de",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "aol.de",
    "gmx.com",
    "zoho.com",
    "tutanota.com",
    "tuta.io",
    "yandex.com",
    "mail.ru",
    "email.de",
    "online.de",
    "vodafone.de",
    "o2online.de",
    "bluewin.ch",
    // Reserved names the tests and the anonymized corpora use for freemail.
    "mail.example",
    "post.example",
    "web.example",
];

const INVOICE_NUMBER_WORDS: &[&str] = &[
    "rechnungsnummer",
    "rechnungsnr",
    "rechnung nr",
    "re-nr",
    "belegnummer",
    "beleg-nr",
    "invoice number",
    "invoice no",
    "invoice #",
    "receipt #",
    "receipt number",
];

const SALES_WORDS: &[&str] = &[
    "rabatt",
    "rabattcode",
    "gutschein",
    "gutscheincode",
    "coupon",
    "promo code",
    "promocode",
    "sale",
    "sonderangebot",
    "angebot",
    "angebote",
    "nur heute",
    "today only",
    "jetzt kaufen",
    "jetzt shoppen",
    "jetzt sichern",
    "shop now",
    "buy now",
    "black friday",
    "cyber monday",
    "deal",
    "deals",
    "sparen",
    "spare",
    "save",
    "% off",
    "bestseller",
    "neuheiten",
    "new arrivals",
    "versandkostenfrei",
    "gratis versand",
    "free shipping",
    "limited time",
    "nur noch",
    "letzte chance",
    "last chance",
    "exklusiv",
    "exclusive",
    "bewerte",
    "bewertung",
    "rate us",
    "leave a review",
    "write a review",
    "wie war",
    "how was your",
    "erfahrung beim",
    "jetzt upgraden",
    "probefahrt",
    "vorteile",
    "jetzt berechnen",
    "jetzt entdecken",
    "jetzt testen",
    "kostenlos testen",
    "try it free",
    "nur für kurze zeit",
    "for a limited time",
    "test drive",
    "vergleichen sie",
    "jetzt vergleichen",
    "unverbindlich",
    "reduziert",
    "günstiger",
    "discount",
    "off everything",
    "auf alles",
];
/// Sales words that need to stand as whole words.
const SALES_WHOLE_WORDS: &[&str] = &["sale", "deal", "deals", "save", "spare", "angebot", "angebote"];

const ACCOUNT_WORDS: &[&str] = &[
    "passwort",
    "kennwort",
    "password",
    "neue anmeldung",
    "anmeldecode",
    "anmeldeversuch",
    "angemeldet",
    "sign-in",
    "sign in",
    "signed in",
    "signin",
    "login",
    "log-in",
    "anmeldung bei",
    "sicherheitscode",
    "bestätigungscode",
    "verifizierungscode",
    "verification code",
    "security code",
    "einmalcode",
    "einmalpasswort",
    "one-time",
    "zwei-faktor",
    "2-faktor",
    "two-factor",
    "2-step",
    "2fa",
    "sicherheitswarnung",
    "sicherheitshinweis",
    "sicherheitsbenachrichtigung",
    "security alert",
    "security notification",
    "security notice",
    "e-mail-adresse bestätigen",
    "e-mail bestätigen",
    "bestätige deine e-mail",
    "bestätigen sie ihre e-mail",
    "confirm your email",
    "confirm your e-mail",
    "verify your email",
    "verify your e-mail",
    "verify your account",
    "confirm your account",
    "verify your",
    "secure link",
    "sicherer link",
    "magic link",
    "anmeldelink",
    "login link",
    "passkey",
    "erfolgreiche anmeldung",
    "zugangsprofil",
    "datenschutzrichtlinie",
    "richtlinie",
    "vertrag",
    "trial",
    "kündigung",
    "vertragsänderung",
    "tarifwechsel",
    "konto bestätigen",
    "konto aktivieren",
    "activate your account",
    "registrierung",
    "willkommen bei",
    "welcome to",
    "dein konto",
    "ihr konto",
    "deinem konto",
    "ihrem konto",
    "your account",
    "konto wurde",
    "zugangsdaten",
    "nutzungsbedingungen",
    "geschäftsbedingungen",
    "agb",
    "datenschutzerklärung",
    "datenschutzbestimmungen",
    "terms of service",
    "terms of use",
    "privacy policy",
    "testzeitraum",
    "testphase",
    "free trial",
    "trial ends",
    "trial period",
    "mitgliedschaft",
    "membership",
    "verbunden",
    "connected to your",
    "e-mail-adresse geändert",
    "email address changed",
    "account recovery",
    "kontowiederherstellung",
];
/// Account words that need to stand as whole words.
const ACCOUNT_WHOLE_WORDS: &[&str] = &["login", "agb", "2fa", "verbunden"];

const CASUAL: &[&str] = &[
    "hi",
    "hey",
    "hallo",
    "huhu",
    "moin",
    "servus",
    "liebe",
    "lieber",
    "ihr lieben",
    "lg",
    "vlg",
    "liebe grüße",
    "liebe grüsse",
    "viele grüße",
    "bis bald",
    "bis dann",
    "drück dich",
    "hab dich lieb",
    "cheers",
    "love",
    "hugs",
    "xoxo",
    "ciao",
    "bussi",
    "see you",
    "talk soon",
];
const FORMAL: &[&str] = &[
    "sehr geehrte",
    "sehr geehrter",
    "mit freundlichen grüßen",
    "mit freundlichen grüssen",
    "freundliche grüße",
    "beste grüße",
    "viele grüße aus dem team",
    "hallo herr",
    "hallo frau",
    "guten tag herr",
    "guten tag frau",
    "liebe frau",
    "lieber herr",
    "hello mr",
    "hello ms",
    "dear mr",
    "dear ms",
    "dear mrs",
    "dear sir",
    "dear madam",
    "kind regards",
    "best regards",
    "yours sincerely",
    "regards,",
    "geschäftsführer",
    "handelsregister",
    "amtsgericht",
    "ust-idnr",
    "vat id",
];

impl Facts {
    /// The facts of `mail`.
    pub fn of(mail: &Mail) -> Facts {
        let subject = fold(&mail.subject);
        let text = fold(&mail.text);
        Facts::of_folded(mail, &subject, &text)
    }

    pub(crate) fn of_folded(mail: &Mail, subject: &str, text: &str) -> Facts {
        let header = |name: &str| mail.header(name);
        let precedence_bulk = header("precedence").is_some_and(|value| matches!(fold(value).as_str(), "bulk" | "list"));
        let automatic = header("auto-submitted").is_some_and(|value| fold(value) != "no")
            || header("x-auto-response-suppress").is_some();
        let domain = mail.from_domain();
        let freemail = FREEMAIL.contains(&domain);
        let same_domain = !freemail
            && !domain.is_empty()
            && mail.to.iter().any(|to| to.rsplit_once('@').is_some_and(|(_, d)| d == domain));
        let both = format!("{subject} {text}");
        let mut sales: Vec<String> = Vec::new();
        for word in SALES_WORDS {
            let found = if SALES_WHOLE_WORDS.contains(word) { has_word(&both, word) } else { both.contains(word) };
            if found
                && sales.len() < 6
                && !sales.iter().any(|known| known.contains(word) || word.contains(known.as_str()))
            {
                sales.push((*word).to_owned());
            }
        }
        if let Some(percent) = percent_off(&both)
            && sales.len() < 6
        {
            sales.push(percent);
        }
        let mut account: Vec<String> = Vec::new();
        for word in ACCOUNT_WORDS {
            let found =
                if ACCOUNT_WHOLE_WORDS.contains(word) { has_word(subject, word) } else { subject.contains(word) };
            if found && account.len() < 4 {
                account.push((*word).to_owned());
            }
        }
        let pdf = mail.attachments.iter().any(|a| crate::detect::is_pdf(&a.name, &a.content_type));
        // Greetings and farewells sit at the start and the end of a text.
        let edges = edges(text);
        let tracking = crate::detect::tracking(mail, subject, text).map(|(c, n)| (c.to_owned(), n));
        let carrier = crate::detect::carrier_in(mail, subject, text).map(str::to_owned);
        let shipment_evidence = tracking.is_some()
            || carrier.is_some()
            || crate::text::find_any(&both, crate::detect::SHIPPING_WORDS).is_some()
            || crate::text::find_any(&both, PICKUP_WORDS).is_some();
        Facts {
            list_unsubscribe: header("list-unsubscribe").is_some(),
            bulk: header("list-id").is_some() || precedence_bulk || header("list-unsubscribe-post").is_some(),
            discussion_list: header("list-post").is_some(),
            automatic,
            sender: sender_kind(mail),
            bounce: matches!(mail.from_local(), "mailer-daemon" | "postmaster")
                || any_in(
                    subject,
                    &[
                        "undelivered mail",
                        "undeliverable",
                        "delivery status notification",
                        "returned to sender",
                        "delivery has failed",
                        "delivery delayed",
                        "unzustellbar",
                        "nicht zustellbar",
                        "zustellung verzögert",
                    ],
                ),
            freemail,
            same_domain,
            authenticated: mail.from_trusted,
            known_sender: mail.known_sender,
            amounts: amounts(text),
            invoice_numbers: invoice_numbers(&both),
            tracking,
            carrier,
            dates: crate::detect::date(&both).into_iter().collect(),
            times: crate::detect::time(&both).into_iter().collect(),
            calendar: mail.calendar,
            pdf,
            code: one_time_code(&both),
            sales,
            account,
            casual: CASUAL.iter().any(|word| has_word(&edges, word)),
            formal: FORMAL.iter().any(|word| text.contains(word)),
            order: crate::text::find_any(subject, crate::detect::ORDER_SUBJECT_WORDS).is_some(),
            invoice_word: crate::text::find_any(subject, crate::detect::INVOICE_STEMS).is_some()
                || mail.attachments.iter().any(|a| {
                    let name = fold(&a.name);
                    name.ends_with(".pdf") && crate::text::find_any(&name, crate::detect::INVOICE_STEMS).is_some()
                }),
            to_self: !mail.from.is_empty() && mail.to.contains(&mail.from),
            notification: header("list-post").is_none() && notification(mail.from_local(), subject),
            test_mail: TEST_SUBJECTS.contains(&subject.trim_matches(|c: char| !c.is_alphanumeric())),
            shipment_evidence,
        }
    }

    /// Sent to many: a list, bulk, or a marketing address.
    pub fn mass_mail(&self) -> bool {
        self.list_unsubscribe || self.bulk || self.sender == SenderKind::Marketing
    }

    /// Written by hand by a person, as far as the headers and the address tell.
    pub fn written_by_person(&self) -> bool {
        self.sender == SenderKind::Person && !self.mass_mail() && !self.automatic && !self.discussion_list
    }

    /// The facts as lines for a model's prompt, in English.
    pub fn for_prompt(&self) -> String {
        let yes = |on: bool| if on { "yes" } else { "no" };
        let list = |items: &[String]| if items.is_empty() { "none".to_owned() } else { items.join(", ") };
        let mut lines = vec![
            format!("sender type: {}", self.sender.as_str()),
            format!("sender is a freemail address: {}", yes(self.freemail)),
            format!("sender has the same domain as the reader (colleague): {}", yes(self.same_domain)),
            format!("sender known to the reader (address book or written to): {}", yes(self.known_sender)),
            format!("sender verified (SPF/DKIM/DMARC): {}", yes(self.authenticated)),
            format!("List-Unsubscribe header: {}", yes(self.list_unsubscribe)),
            format!("sent in bulk (List-Id/Precedence/one-click unsubscribe): {}", yes(self.bulk)),
            format!("discussion list: {}", yes(self.discussion_list)),
            format!("sent automatically (Auto-Submitted): {}", yes(self.automatic)),
            format!("amounts of money: {}", list(&self.amounts)),
            format!("invoice numbers: {}", list(&self.invoice_numbers)),
        ];
        lines.push(match &self.tracking {
            Some((carrier, number)) => format!("tracking number: {carrier} {number}"),
            None => format!("tracking number: none (carrier named: {})", self.carrier.as_deref().unwrap_or("none")),
        });
        lines.push(format!("dates: {}; times: {}", list(&self.dates), list(&self.times)));
        lines.push(format!("calendar invitation attached: {}", yes(self.calendar)));
        lines.push(format!("PDF attached: {}", yes(self.pdf)));
        lines.push(format!("one-time code: {}", if self.code.is_some() { "yes" } else { "no" }));
        lines.push(format!("sales words: {}", list(&self.sales)));
        lines.push(format!("account words in the subject: {}", list(&self.account)));
        lines.push(format!("casual greeting: {}; business greeting: {}", yes(self.casual), yes(self.formal)));
        lines.push(format!("order word in the subject: {}", yes(self.order)));
        lines.push(format!("invoice word in the subject or an invoice PDF: {}", yes(self.invoice_word)));
        lines.push(format!("sent to the sender's own address: {}", yes(self.to_self)));
        lines.push(format!(
            "notification of an app or social network about activity (followers, posts, mentions): {}",
            yes(self.notification)
        ));
        lines.join("\n")
    }
}

/// Parts of a local part that send notifications about activity, not editions: `stories-recap`,
/// `notification`, `follow-suggestions`. "alert" is not among them: job alerts are newsletters.
const NOTIFICATION_LOCAL: &[&str] = &[
    "notification",
    "notifications",
    "notify",
    "notifier",
    "recap",
    "recaps",
    "activity",
    "suggestions",
    "suggestion",
    "reminder",
    "reminders",
    "friends",
    "friendupdates",
];

/// What subjects of activity notifications say (folded).
const NOTIFICATION_SUBJECTS: &[&str] = &[
    "neue follower",
    "neuen follower",
    "new follower",
    "haben vor kurzem",
    "hat vor kurzem",
    "hat etwas gepostet",
    "haben etwas gepostet",
    "hat dich erwahnt",
    "hat dich erwähnt",
    "hat dich markiert",
    "mentioned you",
    "tagged you",
    "liked your",
    "gefallt dein",
    "gefällt dein",
    "commented on",
    "hat kommentiert",
    "hat deinen beitrag",
    "sent you a message",
    "neue nachricht von",
    "new message from",
    "du hast neue",
    "you have new",
    "sieh dir an, was",
    "see what's happening",
    "see what you missed",
    "was du verpasst hast",
    "freundschaftsanfrage",
    "friend request",
    "wants to connect",
    "mochte sich mit dir vernetzen",
    "möchte sich mit dir vernetzen",
    "du hast gerade",
    "you just received",
];

/// Whether a mail from `local` with the folded `subject` is a notification about activity.
fn notification(local: &str, subject: &str) -> bool {
    let parts: Vec<&str> = local.split(['.', '-', '_', '+']).collect();
    parts.iter().any(|part| NOTIFICATION_LOCAL.contains(part))
        || NOTIFICATION_SUBJECTS.iter().any(|phrase| subject.contains(phrase))
}

/// Subjects of test mails, folded and without punctuation.
const TEST_SUBJECTS: &[&str] =
    &["test", "testmail", "test mail", "test-mail", "testing", "test 123", "probe", "testnachricht"];

/// Words of a parcel waiting to be picked up.
const PICKUP_WORDS: &[&str] = &["abholbereit", "abholung", "abholen", "pickup", "pick up", "ready for collection"];

/// The first and the last 300 characters of folded text.
fn edges(text: &str) -> String {
    let chars = text.chars().count();
    if chars <= 600 {
        return text.to_owned();
    }
    let head: String = text.chars().take(300).collect();
    let tail: String = text.chars().skip(chars - 300).collect();
    format!("{head} {tail}")
}

fn sender_kind(mail: &Mail) -> SenderKind {
    let local = mail.from_local();
    if local.is_empty() {
        return SenderKind::Unknown;
    }
    let parts: Vec<&str> = local.split(['.', '-', '_', '+']).collect();
    let local_is = |list: &[&str]| list.contains(&local) || list.iter().any(|word| parts.contains(word));
    if NO_REPLY.iter().any(|word| local.contains(word)) {
        return SenderKind::NoReply;
    }
    if local_is(MARKETING) {
        return SenderKind::Marketing;
    }
    let labels: Vec<&str> = mail.from_domain().split('.').collect();
    if labels.len() >= 3 && MARKETING_SUBDOMAINS.contains(&labels[0]) {
        return SenderKind::Marketing;
    }
    if local_is(ROLE) {
        return SenderKind::Role;
    }
    let name = fold(&mail.from_name);
    let name_words: Vec<&str> = words(&name).map(|(_, word)| word).collect();
    let company_name =
        name_words.iter().any(|word| COMPANY_WORDS.contains(word)) || name.contains(".de") || name.contains(".com");
    if company_name {
        return SenderKind::Role;
    }
    let letters_only = parts.iter().all(|part| part.chars().all(|c| c.is_alphabetic() || c.is_ascii_digit()));
    let digits = local.chars().filter(char::is_ascii_digit).count();
    if !letters_only || digits > 4 || !local.chars().any(char::is_alphabetic) {
        return SenderKind::Unknown;
    }
    // At a freemail provider, every address that is no system's is a person's.
    if FREEMAIL.contains(&mail.from_domain()) {
        return SenderKind::Person;
    }
    // Elsewhere a single word (`prime@`, `auth@`) may as well be a service: only a name in the
    // address that the display name has too, or `first.last` without another display name, is a
    // person's.
    let name_in_address = (2..=4).contains(&name_words.len())
        && name_words.iter().any(|word| word.chars().count() >= 3 && parts.iter().any(|part| part == word));
    let first_last = parts.len() >= 2 && parts.len() <= 3 && parts.iter().all(|part| part.chars().count() >= 1);
    if name_in_address || (first_last && (name_words.is_empty() || name_in_address_loose(&name_words, local))) {
        return SenderKind::Person;
    }
    SenderKind::Unknown
}

/// Whether one of the display name's words (three letters or more) is in the address at all.
fn name_in_address_loose(name_words: &[&str], local: &str) -> bool {
    name_words.iter().any(|word| word.chars().count() >= 3 && local.contains(word))
}

/// Every amount of money in folded text, as written (at most [`MAX_FOUND`]).
fn amounts(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut offset = 0;
    while out.len() < MAX_FOUND {
        let Some(found) = crate::detect::amount(rest) else { break };
        let Some(at) = rest.find(found.as_str()) else { break };
        if !out.contains(&found) {
            out.push(found.clone());
        }
        offset += at + found.len();
        rest = &text[offset..];
    }
    out
}

/// Invoice numbers: the word after "Rechnungsnummer", "Invoice no." and alike, or words like
/// `RE-2026-4711` and `INV-10234`.
fn invoice_numbers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in INVOICE_NUMBER_WORDS {
        let Some(at) = text.find(word) else { continue };
        let after = &text[at + word.len()..];
        let after = after.trim_start_matches(|c: char| c == ':' || c == '.' || c == '#' || c.is_whitespace());
        let number: String =
            after.chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '/').take(30).collect();
        let number = number.trim_end_matches(['-', '/']).to_owned();
        if number.chars().any(|c| c.is_ascii_digit()) && !out.contains(&number) && out.len() < MAX_FOUND {
            out.push(number);
        }
    }
    let list: Vec<(usize, &str)> = words(text).collect();
    for (index, (start, word)) in list.iter().enumerate() {
        if out.len() >= MAX_FOUND {
            break;
        }
        if !matches!(*word, "re" | "inv" | "rg" | "rn") {
            continue;
        }
        let Some(&(next, digits)) = list.get(index + 1) else { continue };
        if text[start + word.len()..next].trim() != "-"
            || digits.len() < 4
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        // `RE-2026-4711`: take a second run of digits too.
        let mut end = next + digits.len();
        if let Some(&(third, more)) = list.get(index + 2)
            && text[end..third].trim() == "-"
            && more.bytes().all(|b| b.is_ascii_digit())
        {
            end = third + more.len();
        }
        let number = text[*start..end].to_uppercase();
        if !out.contains(&number) {
            out.push(number);
        }
    }
    out
}

/// A one-time code: 4 to 8 digits within a few words after `code`, `pin`, `tan` or `passcode`.
fn one_time_code(text: &str) -> Option<String> {
    let list: Vec<(usize, &str)> = words(text).collect();
    for (index, (_, word)) in list.iter().enumerate() {
        let lead = matches!(
            *word,
            "code"
                | "pin"
                | "tan"
                | "passcode"
                | "otp"
                | "sicherheitscode"
                | "bestätigungscode"
                | "verifizierungscode"
                | "anmeldecode"
                | "einmalcode"
                | "zugangscode"
                | "aktivierungscode"
                | "freischaltcode"
        );
        if !lead {
            continue;
        }
        for (_, next) in list.iter().skip(index + 1).take(5) {
            if (4..=8).contains(&next.len()) && next.bytes().all(|b| b.is_ascii_digit()) {
                return Some((*next).to_owned());
            }
        }
    }
    None
}

/// "20 % rabatt", "-30 %", "50% off", "bis zu 70 %" in folded text.
fn percent_off(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = text[from..].find('%') {
        let at = from + found;
        from = at + 1;
        let before = text[..at].trim_end();
        let digits: String = before.chars().rev().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 2 {
            continue;
        }
        let number: String = digits.chars().rev().collect();
        let start = before.len() - number.len();
        let minus = before[..start].ends_with('-');
        let after = text[at + 1..].trim_start();
        let sale_after = ["rabatt", "off", "günstiger", "reduziert", "sparen", "auf alles", "discount", "nachlass"]
            .iter()
            .any(|word| after.starts_with(word));
        let sale_before = ["bis zu", "up to", "spare", "save", "sparen sie"]
            .iter()
            .any(|word| before[..start].trim_end().ends_with(word));
        if minus || sale_after || sale_before {
            return Some(format!("{}{number} %", if minus { "-" } else { "" }));
        }
    }
    None
}

/// Whether `stems` occur in folded `text` (substring), for the detectors.
pub(crate) fn any_in(text: &str, stems: &[&str]) -> bool {
    find_any(text, stems).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(from: &str, name: &str, subject: &str, text: &str) -> Mail {
        let mut mail = Mail::new(from, subject, text, Vec::new(), false, Vec::new());
        mail.from_name = name.to_owned();
        mail
    }

    #[test]
    fn senders() {
        let kind = |from: &str, name: &str| Facts::of(&mail(from, name, "", "")).sender;
        assert_eq!(kind("no-reply@email.shop.example", "Shop"), SenderKind::NoReply);
        assert_eq!(kind("account-security-noreply@accountprotection.example", ""), SenderKind::NoReply);
        assert_eq!(kind("news@shop.example", "Shop"), SenderKind::Marketing);
        assert_eq!(kind("hello@email.shop.example", "Shop"), SenderKind::Marketing);
        assert_eq!(kind("service@bank.example", "Bank"), SenderKind::Role);
        assert_eq!(kind("leni.beispiel@mail.example", "Leni Beispiel"), SenderKind::Person);
        assert_eq!(kind("leni@firma.example", "Leni Beispiel (Firma GmbH)"), SenderKind::Role);
        assert_eq!(kind("prime@shop.example", "Shop Prime"), SenderKind::Role);
        assert_eq!(kind("person7@mail.example", ""), SenderKind::Person);
    }

    #[test]
    fn found_things() {
        let facts = Facts::of(&mail(
            "billing@shop.example",
            "",
            "Rechnung RE-2026-4711",
            "Hallo,\nRechnungsnummer: 2026-0815\nBetrag 49,90 € und 5,00 €. Dein Code: 482913\nNur heute 20 % Rabatt!",
        ));
        assert_eq!(facts.amounts, ["49,90 €", "5,00 €"]);
        assert_eq!(facts.invoice_numbers, ["2026-0815", "RE-2026-4711"]);
        assert_eq!(facts.code.as_deref(), Some("482913"));
        assert!(facts.sales.contains(&"nur heute".to_owned()), "{:?}", facts.sales);
        assert!(facts.sales.contains(&"20 %".to_owned()), "{:?}", facts.sales);
        assert!(facts.casual);
        assert!(!facts.formal);
    }
}
