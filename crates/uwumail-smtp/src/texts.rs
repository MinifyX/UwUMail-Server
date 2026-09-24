//! Words the server writes into mail, per language and tone.
//!
//! Mail to our own people follows the internal tone (playful by default).
//! Mail to anyone else follows the external tone (neutral by default, "light" is
//! friendly but never over the top). `brand` is the name the server goes by, UwUMail unless an
//! admin chose another.

use crate::config::{ExternalTone, InternalTone, Language, ToneConfig};

pub struct BounceTexts {
    pub sender_name: String,
    pub subject: &'static str,
    pub intro: &'static str,
    pub outro: &'static str,
}

/// Who a bounce comes from: the playful post office, the server's delivery, or the anonymous
/// system name other servers use too.
enum Sender {
    PostOffice,
    Delivery,
    System,
}

fn sender(language: Language, sender: Sender, brand: &str) -> String {
    use Language as L;
    match (sender, language) {
        (Sender::System, _) => "Mail Delivery System".into(),
        (Sender::PostOffice, L::De) => format!("Nyu vom {brand}-Postamt"),
        (Sender::PostOffice, L::En) => format!("Nyu from the {brand} post office"),
        (Sender::PostOffice, L::Fr) => format!("Nyu du bureau de poste {brand}"),
        (Sender::PostOffice, L::Nl) => format!("Nyu van het {brand}-postkantoor"),
        (Sender::PostOffice, L::Ja) => format!("{brand} 郵便局のニュウ"),
        (Sender::PostOffice, L::Zh) => format!("{brand} 邮局的 Nyu"),
        (Sender::Delivery, L::De) => format!("{brand} Zustellung"),
        (Sender::Delivery, L::En) => format!("{brand} Delivery"),
        (Sender::Delivery, L::Fr) => format!("Distribution {brand}"),
        (Sender::Delivery, L::Nl) => format!("{brand} Bezorging"),
        (Sender::Delivery, L::Ja) => format!("{brand} 配信"),
        (Sender::Delivery, L::Zh) => format!("{brand} 投递"),
    }
}

pub fn bounce(tone: ToneConfig, recipient_is_local: bool, brand: &str) -> BounceTexts {
    use {ExternalTone as E, InternalTone as I, Language as L};
    let (who, subject, intro, outro) = match (tone.language, recipient_is_local, tone.internal, tone.external) {
        (L::De, true, I::Playful, _) => (
            Sender::PostOffice,
            "Deine Mail ist nicht angekommen (｡•́︿•̀｡)",
            "Oh nein! Ich hab's versucht, aber bei diesen Empfängern kam deine Mail nicht an:",
            "Schau am besten nach, ob sich ein Tippfehler in die Adresse geschlichen hat. \
             Die Kopfzeilen deiner Mail hab ich dir unten angehängt. ♡",
        ),
        (L::De, true, I::Neutral, _) => (
            Sender::Delivery,
            "Unzustellbar: Deine Mail konnte nicht zugestellt werden",
            "Deine Mail konnte an folgende Empfänger nicht zugestellt werden:",
            "Bitte prüfe die Adressen. Die Kopfzeilen der ursprünglichen Mail stehen im Anhang.",
        ),
        (L::De, false, _, E::Neutral) => (
            Sender::System,
            "Unzustellbar: Ihre Nachricht konnte nicht zugestellt werden",
            "Ihre Nachricht konnte an folgende Empfänger nicht zugestellt werden:",
            "Die Kopfzeilen der ursprünglichen Nachricht finden Sie im Anhang.",
        ),
        (L::De, false, _, E::Light) => (
            Sender::System,
            "Ihre Nachricht ist leider nicht angekommen",
            "Hallo! Leider konnten wir Ihre Nachricht an folgende Empfänger nicht zustellen:",
            "Vielleicht hat sich ein kleiner Tippfehler eingeschlichen? \
             Die Kopfzeilen der ursprünglichen Nachricht hängen an. Viele Grüße ✉",
        ),
        (L::En, true, I::Playful, _) => (
            Sender::PostOffice,
            "Your mail didn't make it (｡•́︿•̀｡)",
            "Oh no! I tried my best, but your mail didn't reach these recipients:",
            "Maybe a typo sneaked into the address? I attached the headers of your mail below. ♡",
        ),
        (L::En, true, I::Neutral, _) => (
            Sender::Delivery,
            "Undeliverable: your mail could not be delivered",
            "Your mail could not be delivered to these recipients:",
            "Please check the addresses. The headers of the original mail are attached.",
        ),
        (L::En, false, _, E::Neutral) => (
            Sender::System,
            "Undeliverable: your message could not be delivered",
            "Your message could not be delivered to the following recipients:",
            "The headers of the original message are attached.",
        ),
        (L::En, false, _, E::Light) => (
            Sender::System,
            "Your message didn't arrive",
            "Hi there! Unfortunately we couldn't deliver your message to these recipients:",
            "Maybe a small typo sneaked in? The headers of the original message are attached. Best wishes ✉",
        ),
        (L::Fr, true, I::Playful, _) => (
            Sender::PostOffice,
            "Ton message n'est pas arrivé (｡•́︿•̀｡)",
            "Oh non ! J'ai tout essayé, mais ton message n'a pas atteint ces destinataires :",
            "Vérifie si une faute de frappe ne s'est pas glissée dans l'adresse. \
             Je t'ai joint les en-têtes de ton message ci-dessous. ♡",
        ),
        (L::Fr, true, I::Neutral, _) => (
            Sender::Delivery,
            "Non distribuable : votre message n'a pas pu être remis",
            "Votre message n'a pas pu être remis aux destinataires suivants :",
            "Veuillez vérifier les adresses. Les en-têtes du message d'origine sont joints.",
        ),
        (L::Fr, false, _, E::Neutral) => (
            Sender::System,
            "Non distribuable : votre message n'a pas pu être remis",
            "Votre message n'a pas pu être remis aux destinataires suivants :",
            "Les en-têtes du message d'origine sont joints.",
        ),
        (L::Fr, false, _, E::Light) => (
            Sender::System,
            "Votre message n'est malheureusement pas arrivé",
            "Bonjour ! Nous n'avons malheureusement pas pu remettre votre message à ces destinataires :",
            "Une petite faute de frappe s'est peut-être glissée ? \
             Les en-têtes du message d'origine sont joints. Bien cordialement ✉",
        ),
        (L::Nl, true, I::Playful, _) => (
            Sender::PostOffice,
            "Je mail is niet aangekomen (｡•́︿•̀｡)",
            "O nee! Ik heb het echt geprobeerd, maar je mail kwam niet aan bij deze ontvangers:",
            "Kijk even of er een typefout in het adres is geslopen. \
             De kopregels van je mail heb ik hieronder bijgevoegd. ♡",
        ),
        (L::Nl, true, I::Neutral, _) => (
            Sender::Delivery,
            "Onbestelbaar: je mail kon niet worden bezorgd",
            "Je mail kon niet worden bezorgd bij deze ontvangers:",
            "Controleer de adressen. De kopregels van de oorspronkelijke mail zijn bijgevoegd.",
        ),
        (L::Nl, false, _, E::Neutral) => (
            Sender::System,
            "Onbestelbaar: uw bericht kon niet worden bezorgd",
            "Uw bericht kon niet worden bezorgd bij de volgende ontvangers:",
            "De kopregels van het oorspronkelijke bericht zijn bijgevoegd.",
        ),
        (L::Nl, false, _, E::Light) => (
            Sender::System,
            "Uw bericht is helaas niet aangekomen",
            "Hallo! Helaas konden we uw bericht niet bezorgen bij deze ontvangers:",
            "Misschien is er een kleine typefout ingeslopen? \
             De kopregels van het oorspronkelijke bericht zijn bijgevoegd. Met vriendelijke groet ✉",
        ),
        (L::Ja, true, I::Playful, _) => (
            Sender::PostOffice,
            "メールが届きませんでした (｡•́︿•̀｡)",
            "ごめんなさい！がんばったけど、次の宛先にはメールが届きませんでした：",
            "アドレスに打ち間違いがないか確かめてみてね。元のメールのヘッダーを下に添付しました。♡",
        ),
        (L::Ja, true, I::Neutral, _) => (
            Sender::Delivery,
            "配信不能：メールを配信できませんでした",
            "次の宛先にメールを配信できませんでした：",
            "アドレスをご確認ください。元のメールのヘッダーを添付しています。",
        ),
        (L::Ja, false, _, E::Neutral) => (
            Sender::System,
            "配信不能：メッセージを配信できませんでした",
            "次の宛先にメッセージを配信できませんでした：",
            "元のメッセージのヘッダーを添付しています。",
        ),
        (L::Ja, false, _, E::Light) => (
            Sender::System,
            "メッセージが届きませんでした",
            "こんにちは。申し訳ありませんが、次の宛先にメッセージを配信できませんでした：",
            "アドレスに小さな打ち間違いがあるかもしれません。元のメッセージのヘッダーを添付しています。よろしくお願いいたします ✉",
        ),
        (L::Zh, true, I::Playful, _) => (
            Sender::PostOffice,
            "你的邮件没有送达 (｡•́︿•̀｡)",
            "哎呀！我已经尽力了，可是你的邮件没能送到这些收件人：",
            "看看地址里是不是有拼写错误吧。原邮件的邮件头我附在下面了。♡",
        ),
        (L::Zh, true, I::Neutral, _) => (
            Sender::Delivery,
            "无法投递：你的邮件未能送达",
            "你的邮件无法投递给以下收件人：",
            "请检查地址。原邮件的邮件头已附上。",
        ),
        (L::Zh, false, _, E::Neutral) => {
            (Sender::System, "无法投递：你的邮件未能送达", "你的邮件无法投递给以下收件人：", "原邮件的邮件头已附上。")
        }
        (L::Zh, false, _, E::Light) => (
            Sender::System,
            "很遗憾，你的邮件没有送达",
            "你好！很遗憾，我们无法把你的邮件投递给以下收件人：",
            "也许地址里有个小小的拼写错误？原邮件的邮件头已附上。祝好 ✉",
        ),
    };
    BounceTexts { sender_name: sender(tone.language, who, brand), subject, intro, outro }
}
