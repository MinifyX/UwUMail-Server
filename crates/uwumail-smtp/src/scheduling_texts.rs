//! Words of the mail that carries invitations, answers and cancellations (iMIP, RFC 6047), per
//! language, in the pattern of `texts.rs`. They follow the language of the person on this server
//! who sends them. The mail goes to people elsewhere, so it is written plainly and politely.

use crate::config::Language;

/// What a scheduling mail is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Invitation,
    Update,
    Cancellation,
    Accepted,
    Declined,
    Tentative,
    /// Any other answer, like a delegation.
    Answered,
}

impl Kind {
    /// The kind of answer a `PARTSTAT` gives.
    pub fn of_answer(partstat: &str) -> Kind {
        match partstat.to_ascii_uppercase().as_str() {
            "ACCEPTED" => Kind::Accepted,
            "DECLINED" => Kind::Declined,
            "TENTATIVE" => Kind::Tentative,
            _ => Kind::Answered,
        }
    }
}

pub struct Texts {
    pub subject: String,
    pub body: String,
}

/// The subject and the text part of a scheduling mail. `who` is the sender's name or address,
/// `title`, `when` and `place` describe the event (the last two may be empty).
pub fn scheduling(language: Language, kind: Kind, who: &str, title: &str, when: &str, place: &str) -> Texts {
    use Kind as K;
    use Language as L;
    let untitled = match language {
        L::De => "Termin",
        L::En => "Event",
        L::Fr => "Événement",
        L::Nl => "Afspraak",
        L::Ja => "予定",
        L::Zh => "日程",
    };
    let title = if title.trim().is_empty() { untitled } else { title.trim() };
    let (prefix, sentence) = match (language, kind) {
        (L::De, K::Invitation) => ("Einladung", format!("{who} lädt Sie zu diesem Termin ein:")),
        (L::De, K::Update) => ("Geänderte Einladung", format!("{who} hat diesen Termin geändert:")),
        (L::De, K::Cancellation) => ("Abgesagt", format!("{who} hat diesen Termin abgesagt:")),
        (L::De, K::Accepted) => ("Angenommen", format!("{who} hat die Einladung angenommen:")),
        (L::De, K::Declined) => ("Abgelehnt", format!("{who} hat die Einladung abgelehnt:")),
        (L::De, K::Tentative) => ("Vorbehaltlich", format!("{who} hat die Einladung vorbehaltlich angenommen:")),
        (L::De, K::Answered) => ("Antwort", format!("{who} hat auf die Einladung geantwortet:")),
        (L::En, K::Invitation) => ("Invitation", format!("{who} invites you to this event:")),
        (L::En, K::Update) => ("Updated invitation", format!("{who} changed this event:")),
        (L::En, K::Cancellation) => ("Cancelled", format!("{who} cancelled this event:")),
        (L::En, K::Accepted) => ("Accepted", format!("{who} accepted the invitation:")),
        (L::En, K::Declined) => ("Declined", format!("{who} declined the invitation:")),
        (L::En, K::Tentative) => ("Tentative", format!("{who} tentatively accepted the invitation:")),
        (L::En, K::Answered) => ("Reply", format!("{who} replied to the invitation:")),
        (L::Fr, K::Invitation) => ("Invitation", format!("{who} vous invite à cet événement :")),
        (L::Fr, K::Update) => ("Invitation modifiée", format!("{who} a modifié cet événement :")),
        (L::Fr, K::Cancellation) => ("Annulé", format!("{who} a annulé cet événement :")),
        (L::Fr, K::Accepted) => ("Accepté", format!("{who} a accepté l'invitation :")),
        (L::Fr, K::Declined) => ("Refusé", format!("{who} a refusé l'invitation :")),
        (L::Fr, K::Tentative) => ("Provisoire", format!("{who} a accepté l'invitation sous réserve :")),
        (L::Fr, K::Answered) => ("Réponse", format!("{who} a répondu à l'invitation :")),
        (L::Nl, K::Invitation) => ("Uitnodiging", format!("{who} nodigt u uit voor deze afspraak:")),
        (L::Nl, K::Update) => ("Gewijzigde uitnodiging", format!("{who} heeft deze afspraak gewijzigd:")),
        (L::Nl, K::Cancellation) => ("Geannuleerd", format!("{who} heeft deze afspraak geannuleerd:")),
        (L::Nl, K::Accepted) => ("Geaccepteerd", format!("{who} heeft de uitnodiging geaccepteerd:")),
        (L::Nl, K::Declined) => ("Afgewezen", format!("{who} heeft de uitnodiging afgewezen:")),
        (L::Nl, K::Tentative) => ("Voorlopig", format!("{who} heeft de uitnodiging voorlopig geaccepteerd:")),
        (L::Nl, K::Answered) => ("Antwoord", format!("{who} heeft op de uitnodiging geantwoord:")),
        (L::Ja, K::Invitation) => ("招待", format!("{who} さんからこの予定に招待されました：")),
        (L::Ja, K::Update) => ("変更された招待", format!("{who} さんがこの予定を変更しました：")),
        (L::Ja, K::Cancellation) => ("キャンセル", format!("{who} さんがこの予定をキャンセルしました：")),
        (L::Ja, K::Accepted) => ("承諾", format!("{who} さんが招待を承諾しました：")),
        (L::Ja, K::Declined) => ("辞退", format!("{who} さんが招待を辞退しました：")),
        (L::Ja, K::Tentative) => ("仮承諾", format!("{who} さんが招待を仮承諾しました：")),
        (L::Ja, K::Answered) => ("返信", format!("{who} さんが招待に返信しました：")),
        (L::Zh, K::Invitation) => ("邀请", format!("{who} 邀请你参加此日程：")),
        (L::Zh, K::Update) => ("邀请已更新", format!("{who} 更改了此日程：")),
        (L::Zh, K::Cancellation) => ("已取消", format!("{who} 取消了此日程：")),
        (L::Zh, K::Accepted) => ("已接受", format!("{who} 接受了邀请：")),
        (L::Zh, K::Declined) => ("已拒绝", format!("{who} 拒绝了邀请：")),
        (L::Zh, K::Tentative) => ("暂定", format!("{who} 暂定接受了邀请：")),
        (L::Zh, K::Answered) => ("回复", format!("{who} 回复了邀请：")),
    };
    let (when_label, place_label, footer) = match language {
        L::De => ("Wann", "Wo", "Ihr Kalenderprogramm kann diese Nachricht aus dem Anhang übernehmen."),
        L::En => ("When", "Where", "Your calendar app can take this message from the attachment."),
        L::Fr => ("Quand", "Où", "Votre agenda peut reprendre ce message depuis la pièce jointe."),
        L::Nl => ("Wanneer", "Waar", "Uw agenda-app kan dit bericht uit de bijlage overnemen."),
        L::Ja => ("日時", "場所", "カレンダーアプリで添付ファイルからこのメッセージを取り込めます。"),
        L::Zh => ("时间", "地点", "你的日历应用可以从附件中导入此消息。"),
    };
    let separator = match language {
        L::Ja | L::Zh => "：",
        L::Fr => " : ",
        _ => ": ",
    };
    let mut body = format!("{sentence}\n\n{title}\n");
    if !when.is_empty() {
        body.push_str(&format!("{when_label}{separator}{when}\n"));
    }
    if !place.is_empty() {
        body.push_str(&format!("{place_label}{separator}{place}\n"));
    }
    body.push_str(&format!("\n{footer}\n"));
    let subject_separator = if matches!(language, L::Ja | L::Zh) { "：" } else { ": " };
    Texts { subject: format!("{prefix}{subject_separator}{title}"), body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_says_every_kind() {
        let kinds = [
            Kind::Invitation,
            Kind::Update,
            Kind::Cancellation,
            Kind::Accepted,
            Kind::Declined,
            Kind::Tentative,
            Kind::Answered,
        ];
        for language in Language::ALL {
            for kind in kinds {
                let texts = scheduling(language, kind, "Mini", "Kaffee", "2026-09-20 09:00", "Café");
                assert!(texts.subject.contains("Kaffee"), "{language:?} {kind:?}");
                assert!(texts.body.contains("Mini") && texts.body.contains("2026-09-20 09:00"));
                assert!(texts.body.contains("Café"));
            }
        }
        let texts = scheduling(Language::De, Kind::Invitation, "Mini", " ", "", "");
        assert_eq!(texts.subject, "Einladung: Termin");
        assert_eq!(Kind::of_answer("accepted"), Kind::Accepted);
    }
}
