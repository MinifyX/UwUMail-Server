//! Words the server writes into mail, per language and tone.
//!
//! Mail to our own people follows the internal tone (playful by default).
//! Mail to anyone else follows the external tone (neutral by default, "light" is
//! friendly but never over the top).

use crate::config::{ExternalTone, InternalTone, Language, ToneConfig};

pub struct BounceTexts {
    pub sender_name: &'static str,
    pub subject: &'static str,
    pub intro: &'static str,
    pub outro: &'static str,
}

pub fn bounce(tone: ToneConfig, recipient_is_local: bool) -> BounceTexts {
    use {ExternalTone as E, InternalTone as I, Language as L};
    match (tone.language, recipient_is_local, tone.internal, tone.external) {
        (L::De, true, I::Playful, _) => BounceTexts {
            sender_name: "Nyu vom UwUMail-Postamt",
            subject: "Deine Mail ist nicht angekommen (｡•́︿•̀｡)",
            intro: "Oh nein! Ich hab's versucht, aber bei diesen Empfängern kam deine Mail nicht an:",
            outro: "Schau am besten nach, ob sich ein Tippfehler in die Adresse geschlichen hat. \
                    Die Kopfzeilen deiner Mail hab ich dir unten angehängt. ♡",
        },
        (L::De, true, I::Neutral, _) => BounceTexts {
            sender_name: "UwUMail Zustellung",
            subject: "Unzustellbar: Deine Mail konnte nicht zugestellt werden",
            intro: "Deine Mail konnte an folgende Empfänger nicht zugestellt werden:",
            outro: "Bitte prüfe die Adressen. Die Kopfzeilen der ursprünglichen Mail stehen im Anhang.",
        },
        (L::De, false, _, E::Neutral) => BounceTexts {
            sender_name: "Mail Delivery System",
            subject: "Unzustellbar: Ihre Nachricht konnte nicht zugestellt werden",
            intro: "Ihre Nachricht konnte an folgende Empfänger nicht zugestellt werden:",
            outro: "Die Kopfzeilen der ursprünglichen Nachricht finden Sie im Anhang.",
        },
        (L::De, false, _, E::Light) => BounceTexts {
            sender_name: "Mail Delivery System",
            subject: "Ihre Nachricht ist leider nicht angekommen",
            intro: "Hallo! Leider konnten wir Ihre Nachricht an folgende Empfänger nicht zustellen:",
            outro: "Vielleicht hat sich ein kleiner Tippfehler eingeschlichen? \
                    Die Kopfzeilen der ursprünglichen Nachricht hängen an. Viele Grüße ✉",
        },
        (L::En, true, I::Playful, _) => BounceTexts {
            sender_name: "Nyu from the UwUMail post office",
            subject: "Your mail didn't make it (｡•́︿•̀｡)",
            intro: "Oh no! I tried my best, but your mail didn't reach these recipients:",
            outro: "Maybe a typo sneaked into the address? I attached the headers of your mail below. ♡",
        },
        (L::En, true, I::Neutral, _) => BounceTexts {
            sender_name: "UwUMail Delivery",
            subject: "Undeliverable: your mail could not be delivered",
            intro: "Your mail could not be delivered to these recipients:",
            outro: "Please check the addresses. The headers of the original mail are attached.",
        },
        (L::En, false, _, E::Neutral) => BounceTexts {
            sender_name: "Mail Delivery System",
            subject: "Undeliverable: your message could not be delivered",
            intro: "Your message could not be delivered to the following recipients:",
            outro: "The headers of the original message are attached.",
        },
        (L::En, false, _, E::Light) => BounceTexts {
            sender_name: "Mail Delivery System",
            subject: "Your message didn't arrive",
            intro: "Hi there! Unfortunately we couldn't deliver your message to these recipients:",
            outro: "Maybe a small typo sneaked in? The headers of the original message are attached. Best wishes ✉",
        },
    }
}
