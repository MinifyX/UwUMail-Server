//! The base labels (docs/labels.md, "Base labels"): eight fixed labels every person has, each with
//! a sharp definition that does not overlap with the others, examples of what belongs in it and
//! what does not, and the detector that puts it on without a model.
//!
//! The definitions are the same for the deciding without a model, the model's prompt and the people
//! reading them in the settings; only the names follow the person's language.

use crate::Detector;

/// One of the base labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Base {
    Invoice,
    Shipping,
    Appointment,
    Newsletter,
    Account,
    Personal,
    Work,
    Advertising,
}

/// What a base label is, in one language.
#[derive(Debug, Clone, Copy)]
pub struct BaseText {
    pub name: &'static str,
    /// The definition: what belongs in it, and what does not.
    pub description: &'static str,
    /// Mails that belong in it.
    pub examples: &'static [&'static str],
    /// Mails that look alike but do not, each with where they go instead.
    pub counter_examples: &'static [&'static str],
}

/// Names a person may have given a label before that mean the same base label: they are adopted
/// (lower case; compared folded). The base label's own names in every language count too.
const ALIASES: &[(Base, &[&str])] = &[
    (
        Base::Invoice,
        &[
            "rechnungen",
            "rechnung",
            "invoices",
            "invoice",
            "receipts",
            "quittungen",
            "factures",
            "facturen",
            "請求書",
            "账单",
        ],
    ),
    (
        Base::Shipping,
        &[
            "versand",
            "bestellungen & versand",
            "bestellungen",
            "lieferungen",
            "orders & shipping",
            "orders",
            "shipping",
            "commandes & livraisons",
            "bestellingen & verzending",
            "注文と配送",
            "订单与物流",
        ],
    ),
    (
        Base::Appointment,
        &["termine", "termin", "appointments", "appointment", "rendez-vous", "afspraken", "予定", "预约"],
    ),
    (
        Base::Newsletter,
        &["newsletter", "newsletters", "lettres d'information", "nieuwsbrieven", "ニュースレター", "新闻通讯"],
    ),
    (
        Base::Account,
        &[
            "konto & sicherheit",
            "konto und sicherheit",
            "sicherheit",
            "account & security",
            "account and security",
            "security",
        ],
    ),
    (Base::Personal, &["persönlich", "personal", "privat", "private", "personnel", "persoonlijk", "個人", "个人"]),
    (
        Base::Work,
        &[
            "arbeit/geschäftlich",
            "arbeit",
            "geschäftlich",
            "arbeit & geschäftliches",
            "work",
            "work & business",
            "business",
            "travail",
            "werk",
            "仕事",
            "工作",
        ],
    ),
    (
        Base::Advertising,
        &["werbung", "angebote", "promotions", "advertising", "ads", "offers", "publicité", "reclame", "広告", "广告"],
    ),
];

impl Base {
    pub const ALL: [Base; 8] = [
        Base::Invoice,
        Base::Shipping,
        Base::Appointment,
        Base::Newsletter,
        Base::Account,
        Base::Personal,
        Base::Work,
        Base::Advertising,
    ];

    /// The version of the set of base labels that brought it (the store's `BASE_LABELS_VERSION`):
    /// a person who had an earlier set gets it made, one who deleted it keeps it deleted.
    pub fn since(self) -> i64 {
        1
    }

    /// How it is spelled in the store, JMAP and the corpus.
    pub fn as_str(self) -> &'static str {
        match self {
            Base::Invoice => "invoice",
            Base::Shipping => "shipping",
            Base::Appointment => "appointment",
            Base::Newsletter => "newsletter",
            Base::Account => "account",
            Base::Personal => "personal",
            Base::Work => "work",
            Base::Advertising => "advertising",
        }
    }

    pub fn parse(name: &str) -> Option<Base> {
        Base::ALL.into_iter().find(|base| base.as_str() == name)
    }

    /// The detector that puts it on without a model.
    pub fn detector(self) -> Detector {
        match self {
            Base::Invoice => Detector::Invoice,
            Base::Shipping => Detector::Shipping,
            Base::Appointment => Detector::Appointment,
            Base::Newsletter => Detector::Newsletter,
            Base::Account => Detector::Account,
            Base::Personal => Detector::Personal,
            Base::Work => Detector::Work,
            Base::Advertising => Detector::Advertising,
        }
    }

    /// The color it starts with.
    pub fn color(self) -> &'static str {
        match self {
            Base::Invoice => "#f59e0b",
            Base::Shipping => "#0ea5e9",
            Base::Appointment => "#8b5cf6",
            Base::Newsletter => "#64748b",
            Base::Account => "#ef4444",
            Base::Personal => "#ec4899",
            Base::Work => "#10b981",
            Base::Advertising => "#a3a3a3",
        }
    }

    /// Base labels a mail never has together: the definitions exclude each other.
    pub fn excludes(self, other: Base) -> bool {
        use Base::*;
        let pair = |a: Base, b: Base| (self == a && other == b) || (self == b && other == a);
        // A person writes it, or a system or a company sends it in bulk: never both.
        let written = |b: Base| matches!(b, Personal | Work);
        let bulk = |b: Base| matches!(b, Newsletter | Advertising);
        self != other
            && (pair(Personal, Work)
                || pair(Newsletter, Advertising)
                || (written(self) && !matches!(other, Appointment) && !written(other))
                || (written(other) && !matches!(self, Appointment) && !written(self))
                || (bulk(self) && matches!(other, Invoice | Shipping | Account | Appointment))
                || (bulk(other) && matches!(self, Invoice | Shipping | Account | Appointment))
                || pair(Account, Shipping)
                || pair(Account, Appointment))
    }

    /// Whether a label called `name` (any case) means this base label.
    pub fn named(name: &str) -> Option<Base> {
        let folded = crate::text::fold(name);
        for base in Base::ALL {
            if ["de", "en", "fr", "nl", "ja", "zh"].iter().any(|lang| crate::text::fold(base.name(lang)) == folded) {
                return Some(base);
            }
        }
        ALIASES.iter().find(|(_, names)| names.contains(&folded.as_str())).map(|(base, _)| *base)
    }

    /// The name in a language (`de`, `en`, `fr`, `nl`, `ja`, `zh`; others get English).
    pub fn name(self, language: &str) -> &'static str {
        let names: [&str; 6] = match self {
            Base::Invoice => ["Rechnung", "Invoice", "Facture", "Factuur", "請求書", "账单"],
            Base::Shipping => ["Versand", "Shipping", "Livraison", "Verzending", "配送", "物流"],
            Base::Appointment => ["Termin", "Appointment", "Rendez-vous", "Afspraak", "予定", "预约"],
            Base::Newsletter => {
                ["Newsletter", "Newsletter", "Lettre d'info", "Nieuwsbrief", "ニュースレター", "新闻通讯"]
            }
            Base::Account => [
                "Konto & Sicherheit",
                "Account & security",
                "Compte & sécurité",
                "Account & beveiliging",
                "アカウントとセキュリティ",
                "账户与安全",
            ],
            Base::Personal => ["Persönlich", "Personal", "Personnel", "Persoonlijk", "個人", "个人"],
            Base::Work => ["Arbeit/Geschäftlich", "Work & business", "Travail", "Werk & zakelijk", "仕事", "工作"],
            Base::Advertising => ["Werbung", "Promotions", "Publicité", "Reclame", "広告", "广告"],
        };
        let index = match language {
            "de" => 0,
            "fr" => 2,
            "nl" => 3,
            "ja" => 4,
            "zh" => 5,
            _ => 1,
        };
        names[index]
    }

    /// The definition with examples, in German for `de` and in English otherwise; the name in
    /// `language`.
    pub fn text(self, language: &str) -> BaseText {
        let mut text = if language == "de" { self.german() } else { self.english() };
        text.name = self.name(language);
        text
    }

    fn german(self) -> BaseText {
        match self {
            Base::Invoice => BaseText {
                name: "",
                description: "Ein Beleg über Geld, das du schuldest oder bezahlt hast: Rechnung, Quittung, Zahlungsbestätigung, Mahnung, Gutschrift, Kontoabbuchung mit Betrag. Nicht: Bestellbestätigungen ohne Rechnung (Versand), Werbung mit Preisen (Werbung).",
                examples: &[
                    "Ihre Mobilfunkrechnung September: 39,99 €, Abbuchung am 05.10.",
                    "Zahlungsbestätigung: 12,00 € an Beispiel-Verein bezahlt",
                    "Rechnung RE-2026-4711 als PDF im Anhang",
                ],
                counter_examples: &[
                    "Danke für deine Bestellung, wir packen sie (Versand)",
                    "Nur heute 20 % auf alles (Werbung)",
                    "Deine Testphase endet bald (Konto & Sicherheit)",
                ],
            },
            Base::Shipping => BaseText {
                name: "",
                description: "Bestellte Waren und ihr Weg zu dir: Bestellbestätigung, versandt, Sendungsverfolgung, Zustellung, Abholung, Rücksendung. Nicht: die Rechnung oder der Beleg zur Bestellung (Rechnung), Bestellungen ohne Versand wie Abos und digitale Käufe (Rechnung oder Konto & Sicherheit), Werbung über kostenlosen Versand (Werbung).",
                examples: &[
                    "Ihr Paket ist unterwegs, Sendungsnummer 00340434161234567890",
                    "In Zustellung: deine Bestellung kommt heute",
                    "Bestellbestätigung Nr. 302-1234567",
                ],
                counter_examples: &[
                    "Gratis Versand ab 20 € – nur dieses Wochenende (Werbung)",
                    "Rechnung zu Ihrer Bestellung im Anhang (Rechnung)",
                ],
            },
            Base::Appointment => BaseText {
                name: "",
                description: "Ein fester Termin, zu dem du gehst oder an dem du teilnimmst: Termin, Einladung, Besprechung, Reservierung, Ticket; auch Bestätigung, Erinnerung, Verschiebung, Absage. Nicht: Liefertermine (Versand), Zahlungsfristen (Rechnung), beworbene Veranstaltungen und Webinare (Werbung).",
                examples: &[
                    "Terminbestätigung: Zahnarzt am Di, 06.10. um 09:30",
                    "Einladung: Projektbesprechung Donnerstag 14 Uhr",
                    "Ihre Tischreservierung für Samstag, 19:00",
                ],
                counter_examples: &[
                    "Zustellung voraussichtlich Montag (Versand)",
                    "Melde dich jetzt zu unserem Webinar an (Werbung)",
                ],
            },
            Base::Newsletter => BaseText {
                name: "",
                description: "Regelmäßige Ausgaben mit Inhalten, die du abonniert hast: Nachrichten, Wochenrückblick, Blog, Neuigkeiten eines Projekts oder Vereins, Job-Alerts. Nicht: Mails, die vor allem verkaufen wollen (Werbung), Mitteilungen zu deinem Konto oder deinen Bestellungen, Benachrichtigungen von Apps und sozialen Netzwerken (neue Follower, Likes, Aktivität).",
                examples: &[
                    "Self-Host Weekly – die Neuigkeiten dieser Woche",
                    "Vereinsnachrichten Oktober",
                    "Neue Jobs für dich: 5 Stellen als Systemadministrator",
                ],
                counter_examples: &[
                    "Sale: bis zu 50 % Rabatt (Werbung)",
                    "Wir ändern unsere AGB für dein Konto (Konto & Sicherheit)",
                    "lea und 3 weitere Personen haben etwas Neues gepostet (kein Label)",
                ],
            },
            Base::Account => BaseText {
                name: "",
                description: "Dein eigenes Konto bei einem Dienst: Registrierung und Willkommen, E-Mail bestätigen, Anmeldecode, Passwort zurücksetzen, neue Anmeldung, Sicherheitswarnung, Änderungen an Konto, Tarif, AGB oder Datenschutz. Nicht: Werbung desselben Dienstes (Werbung), Rechnungen (Rechnung).",
                examples: &[
                    "Dein Bestätigungscode lautet 482913",
                    "Neue Anmeldung bei deinem Konto von Firefox unter Linux",
                    "Bitte bestätige deine E-Mail-Adresse",
                ],
                counter_examples: &[
                    "Upgrade jetzt auf Premium und spare 30 % (Werbung)",
                    "Ihre Rechnung für Oktober (Rechnung)",
                ],
            },
            Base::Personal => BaseText {
                name: "",
                description: "Von einem privaten Menschen an dich persönlich geschrieben: Freunde, Familie, Bekannte. Nicht massenhaft, nicht von einer Firma oder einem System. Nicht: Mails von Firmen mit deinem Namen in der Anrede, Benachrichtigungen, Kolleginnen und Geschäftliches (Arbeit/Geschäftlich).",
                examples: &["Hi, kommst du Samstag zum Grillen? LG Leni", "Fotos vom Urlaub – Papa"],
                counter_examples: &[
                    "Max, deine virtuelle Karte ist bereit (Konto & Sicherheit)",
                    "In Zustellung: deine Bestellung (Versand)",
                    "Hallo Herr Muster, anbei das Angebot (Arbeit/Geschäftlich)",
                ],
            },
            Base::Work => BaseText {
                name: "",
                description: "Von einem Menschen beruflich an dich geschrieben: Kolleginnen, Kundschaft, Geschäftspartner, Bewerbungen, Angebote, die du angefragt hast, Behörden. Nicht: automatische Benachrichtigungen, Newsletter, Privates (Persönlich).",
                examples: &[
                    "Hallo Herr Muster, anbei wie besprochen unser Angebot",
                    "Kurze Frage zum Release morgen – Grüße aus dem Team",
                ],
                counter_examples: &[
                    "[Projekt] Build fehlgeschlagen (keins der Labels)",
                    "Neue Jobs für dich (Newsletter)",
                ],
            },
            Base::Advertising => BaseText {
                name: "",
                description: "Mails, die vor allem etwas verkaufen wollen: Angebote, Rabatte, Sale, Gutscheine, Produktwerbung, Bitten um Bewertungen. Nicht: abonnierte Inhalte ohne Verkaufsabsicht (Newsletter), Bestellung, Versand oder Rechnung eines echten Kaufs.",
                examples: &["Nur heute: 20 % auf alles mit dem Code HERBST20", "Wie war dein Kauf? Bewerte uns"],
                counter_examples: &["Wochenrückblick unseres Blogs (Newsletter)", "Dein Paket ist unterwegs (Versand)"],
            },
        }
    }

    fn english(self) -> BaseText {
        match self {
            Base::Invoice => BaseText {
                name: "",
                description: "A document about money you owe or paid: invoice, receipt, payment confirmation, payment reminder, credit note, a debit with its amount. Not: order confirmations without an invoice (Shipping), ads with prices (Promotions).",
                examples: &[
                    "Your phone bill for September: €39.99, debited on 5 October",
                    "Payment confirmation: $12.00 paid to Example Club",
                    "Invoice INV-2026-4711 attached as PDF",
                ],
                counter_examples: &[
                    "Thanks for your order, we're packing it (Shipping)",
                    "Today only: 20% off everything (Promotions)",
                    "Your trial ends soon (Account & security)",
                ],
            },
            Base::Shipping => BaseText {
                name: "",
                description: "Ordered goods and their way to you: order confirmation, shipped, tracking, delivery, pickup, returns. Not: the invoice or receipt for the order (Invoice), orders with nothing to ship like subscriptions and digital purchases (Invoice or Account & security), ads about free shipping (Promotions).",
                examples: &[
                    "Your package is on its way, tracking number 1Z999AA10123456784",
                    "Out for delivery: your order arrives today",
                    "Order confirmation #302-1234567",
                ],
                counter_examples: &[
                    "Free shipping over $20 – this weekend only (Promotions)",
                    "Invoice for your order attached (Invoice)",
                ],
            },
            Base::Appointment => BaseText {
                name: "",
                description: "A fixed date you go to or take part in: appointment, invitation, meeting, reservation, ticket; also its confirmation, reminder, rescheduling or cancellation. Not: delivery dates (Shipping), payment deadlines (Invoice), advertised events and webinars (Promotions).",
                examples: &[
                    "Appointment confirmed: dentist on Tue, Oct 6 at 9:30 am",
                    "Invitation: project meeting Thursday 2 pm",
                    "Your table reservation for Saturday, 7 pm",
                ],
                counter_examples: &["Estimated delivery Monday (Shipping)", "Sign up for our webinar now (Promotions)"],
            },
            Base::Newsletter => BaseText {
                name: "",
                description: "Regular issues with content you subscribed to: news, weekly digests, blog posts, updates of a project or club, job alerts. Not: mail mainly selling something (Promotions), messages about your account or your orders, notifications of apps and social networks (new followers, likes, activity).",
                examples: &[
                    "Self-Host Weekly – this week's news",
                    "Club news for October",
                    "New jobs for you: 5 openings as a system administrator",
                ],
                counter_examples: &[
                    "Sale: up to 50% off (Promotions)",
                    "We're updating the terms of your account (Account & security)",
                    "lea and 3 others posted something new (no label)",
                ],
            },
            Base::Account => BaseText {
                name: "",
                description: "Your own account at a service: sign-up and welcome, confirming your e-mail, login codes, password reset, new sign-in, security alerts, changes to the account, plan, terms or privacy policy. Not: the same service's ads (Promotions), invoices (Invoice).",
                examples: &[
                    "Your verification code is 482913",
                    "New sign-in to your account from Firefox on Linux",
                    "Please confirm your e-mail address",
                ],
                counter_examples: &[
                    "Upgrade to Premium now and save 30% (Promotions)",
                    "Your invoice for October (Invoice)",
                ],
            },
            Base::Personal => BaseText {
                name: "",
                description: "Written to you personally by a private person: friends, family, acquaintances. Not sent in bulk, not by a company or a system. Not: company mail that greets you by name, notifications, colleagues and business (Work & business).",
                examples: &["Hi, are you coming to the barbecue on Saturday? Cheers, Leni", "Holiday photos – Dad"],
                counter_examples: &[
                    "Max, your virtual card is ready (Account & security)",
                    "Out for delivery: your order (Shipping)",
                    "Dear Mr Muster, please find our quote attached (Work & business)",
                ],
            },
            Base::Work => BaseText {
                name: "",
                description: "Written to you by a person in a professional context: colleagues, customers, business partners, job applications, quotes you asked for, authorities. Not: automated notifications, newsletters, private mail (Personal).",
                examples: &[
                    "Dear Mr Muster, as discussed please find our quote attached",
                    "Quick question about tomorrow's release – regards from the team",
                ],
                counter_examples: &["[project] Build failed (none of the labels)", "New jobs for you (Newsletter)"],
            },
            Base::Advertising => BaseText {
                name: "",
                description: "Mail mainly meant to sell something: offers, discounts, sales, coupons, product promotion, requests for reviews. Not: subscribed content without a sales pitch (Newsletter), the order, shipping or invoice of a real purchase.",
                examples: &["Today only: 20% off everything with code FALL20", "How was your purchase? Rate us"],
                counter_examples: &["Our blog's weekly digest (Newsletter)", "Your package is on its way (Shipping)"],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_exclusions() {
        assert_eq!(Base::named("Rechnungen"), Some(Base::Invoice));
        assert_eq!(Base::named("Bestellungen & Versand"), Some(Base::Shipping));
        assert_eq!(Base::named("  persönlich "), Some(Base::Personal));
        assert_eq!(Base::named("Account & Security"), Some(Base::Account));
        assert_eq!(Base::named("Coding"), None);
        assert!(Base::Personal.excludes(Base::Invoice));
        assert!(Base::Newsletter.excludes(Base::Advertising));
        assert!(!Base::Invoice.excludes(Base::Shipping));
        assert!(!Base::Work.excludes(Base::Appointment));
        assert!(!Base::Invoice.excludes(Base::Account));
        for a in Base::ALL {
            assert!(!a.excludes(a));
            for b in Base::ALL {
                assert_eq!(a.excludes(b), b.excludes(a));
            }
        }
    }
}
