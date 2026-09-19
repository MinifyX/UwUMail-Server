//! Security notices: a short mail into the person's own inbox whenever something about their
//! login changes, so a stranger who took over an account cannot do it unnoticed.

use mail_builder::MessageBuilder;
use mail_builder::headers::date::Date;
use serde_json::Value;
use uwumail_smtp::Language;
use uwumail_store::{Account, IngestRequest, MailboxRole, MailboxTarget, SecurityEvent};

use crate::Web;

/// Which notice to send. The names match the security event kinds.
#[derive(Debug, Clone)]
pub enum Notice {
    PasswordChanged,
    PasswordChosenWithLink,
    PasswordSetByAdmin,
    TotpEnabled,
    TotpDisabled,
    PasskeyAdded { name: String },
    PasskeyRemoved { name: String },
    RecoveryCodesCreated,
    RecoveryCodeUsed { left: i64 },
    AppPasswordCreated { name: String },
    AppsMayUseMainPassword,
    SecondFactorsReset,
    ForwardingAdded { address: String },
}

impl Notice {
    pub fn kind(&self) -> &'static str {
        match self {
            Notice::PasswordChanged => "passwordChanged",
            Notice::PasswordChosenWithLink => "passwordChosenWithLink",
            Notice::PasswordSetByAdmin => "passwordSetByAdmin",
            Notice::TotpEnabled => "totpEnabled",
            Notice::TotpDisabled => "totpDisabled",
            Notice::PasskeyAdded { .. } => "passkeyAdded",
            Notice::PasskeyRemoved { .. } => "passkeyRemoved",
            Notice::RecoveryCodesCreated => "recoveryCodesCreated",
            Notice::RecoveryCodeUsed { .. } => "recoveryCodeUsed",
            Notice::AppPasswordCreated { .. } => "appPasswordCreated",
            Notice::AppsMayUseMainPassword => "appsMayUseMainPassword",
            Notice::SecondFactorsReset => "secondFactorsReset",
            Notice::ForwardingAdded { .. } => "forwardingAdded",
        }
    }

    fn details(&self) -> Value {
        match self {
            Notice::PasskeyAdded { name } | Notice::PasskeyRemoved { name } | Notice::AppPasswordCreated { name } => {
                serde_json::json!({ "name": name })
            }
            Notice::RecoveryCodeUsed { left } => serde_json::json!({ "left": left }),
            Notice::ForwardingAdded { address } => serde_json::json!({ "address": address }),
            _ => Value::Object(Default::default()),
        }
    }

    /// Subject and the sentence saying what happened.
    fn text(&self, language: Language, login: &str, actor: &str) -> (&'static str, String) {
        match language {
            Language::De => match self {
                Notice::PasswordChanged => {
                    ("Dein Passwort wurde geändert", format!("das Passwort deines Kontos {login} wurde geändert."))
                }
                Notice::PasswordChosenWithLink => (
                    "Neues Passwort über einen Link",
                    format!("für dein Konto {login} wurde über einen Einmal-Link ein neues Passwort gewählt."),
                ),
                Notice::PasswordSetByAdmin => {
                    ("Dein Passwort wurde neu gesetzt", format!("{actor} hat ein neues Passwort für dein Konto {login} gesetzt."))
                }
                Notice::TotpEnabled => (
                    "Authenticator-App eingeschaltet",
                    "die Anmeldung mit einer Authenticator-App wurde eingeschaltet. Mail-Apps brauchen ab jetzt App-Passwörter."
                        .into(),
                ),
                Notice::TotpDisabled => {
                    ("Authenticator-App ausgeschaltet", "die Anmeldung mit einer Authenticator-App wurde ausgeschaltet.".into())
                }
                Notice::PasskeyAdded { name } => {
                    ("Neuer Passkey", format!("für dein Konto wurde der Passkey „{name}“ hinzugefügt."))
                }
                Notice::PasskeyRemoved { name } => {
                    ("Passkey entfernt", format!("der Passkey „{name}“ wurde von deinem Konto entfernt."))
                }
                Notice::RecoveryCodesCreated => (
                    "Neue Wiederherstellungscodes",
                    "für dein Konto wurden neue Wiederherstellungscodes erstellt. Die alten gelten nicht mehr.".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "Wiederherstellungscode benutzt",
                    format!("bei einer Anmeldung wurde ein Wiederherstellungscode benutzt. Übrig sind noch {left}."),
                ),
                Notice::AppPasswordCreated { name } => {
                    ("Neues App-Passwort", format!("für dein Konto wurde das App-Passwort „{name}“ erstellt."))
                }
                Notice::AppsMayUseMainPassword => (
                    "Mail-Apps dürfen das Hauptpasswort nutzen",
                    "Mail-Apps können sich wieder mit deinem Hauptpasswort anmelden, nicht nur mit App-Passwörtern.".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "Neue Weiterleitung",
                    format!("für dein Konto wurde eine Weiterleitung an {address} eingerichtet. Adressen auf anderen Servern müssen sie noch bestätigen."),
                ),
                Notice::SecondFactorsReset => (
                    "Zwei-Faktor-Anmeldung zurückgesetzt",
                    format!("{actor} hat die Zwei-Faktor-Anmeldung deines Kontos zurückgesetzt. Du meldest dich jetzt nur mit deinem Passwort an."),
                ),
            },
            Language::En => match self {
                Notice::PasswordChanged => {
                    ("Your password was changed", format!("the password of your account {login} was changed."))
                }
                Notice::PasswordChosenWithLink => (
                    "New password through a link",
                    format!("a new password for your account {login} was chosen through a one-time link."),
                ),
                Notice::PasswordSetByAdmin => {
                    ("Your password was reset", format!("{actor} set a new password for your account {login}."))
                }
                Notice::TotpEnabled => (
                    "Authenticator app turned on",
                    "logging in with an authenticator app was turned on. Mail apps need app passwords from now on.".into(),
                ),
                Notice::TotpDisabled => {
                    ("Authenticator app turned off", "logging in with an authenticator app was turned off.".into())
                }
                Notice::PasskeyAdded { name } => ("New passkey", format!("the passkey “{name}” was added to your account.")),
                Notice::PasskeyRemoved { name } => {
                    ("Passkey removed", format!("the passkey “{name}” was removed from your account."))
                }
                Notice::RecoveryCodesCreated => (
                    "New recovery codes",
                    "new recovery codes were created for your account. The old ones no longer work.".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "Recovery code used",
                    format!("a recovery code was used to log in. {left} are left."),
                ),
                Notice::AppPasswordCreated { name } => {
                    ("New app password", format!("the app password “{name}” was created for your account."))
                }
                Notice::AppsMayUseMainPassword => (
                    "Mail apps may use the main password",
                    "mail apps can log in with your main password again, not only with app passwords.".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "New forwarding",
                    format!("forwarding to {address} was set up for your account. Addresses on other servers still have to confirm it."),
                ),
                Notice::SecondFactorsReset => (
                    "Two-factor login reset",
                    format!("{actor} reset the two-factor login of your account. You now log in with your password only."),
                ),
            },
        }
    }
}

/// Who did it and from where, for the activity list and the notice.
pub struct Origin<'a> {
    /// Empty when the person did it themselves.
    pub actor: &'a str,
    pub ip: &'a str,
}

fn language(web: &Web, preferences: &serde_json::Map<String, Value>) -> Language {
    match preferences.get("language").and_then(Value::as_str) {
        Some("de") => Language::De,
        Some("en") => Language::En,
        _ => web.smtp().tone().language,
    }
}

/// `2026-09-15 14:36 UTC`
fn when(timestamp: i64) -> String {
    let iso = uwumail_jmap::dates::format(timestamp);
    format!("{} {} UTC", &iso[..10], &iso[11..16])
}

/// Writes the event to the person's activity list and puts a notice into their inbox.
/// Failures are logged; the change itself already happened.
pub async fn notify(web: &Web, account: &Account, notice: Notice, origin: Origin<'_>) {
    let event = SecurityEvent {
        kind: notice.kind().into(),
        actor: origin.actor.into(),
        ip: origin.ip.into(),
        details: notice.details(),
    };
    if let Err(err) = web.store().record_security_event(account.id, event).await {
        tracing::error!(%err, login = %account.login, "writing the security activity failed");
    }

    let preferences = web.store().preferences(account.id).await.unwrap_or_default();
    let language = language(web, &preferences);
    let (subject, sentence) = notice.text(language, &account.login, origin.actor);
    let sentence = if language == Language::En { sentence_case(sentence) } else { sentence };
    let hostname = &web.settings().hostname;
    let name =
        if account.display_name.trim().is_empty() { account.login.as_str() } else { account.display_name.trim() };
    let ip_line = if origin.ip.is_empty() { String::new() } else { format!("\nIP: {}", origin.ip) };
    let now = crate::health::unix_now();
    let body = match language {
        Language::De => format!(
            "Hallo {name},\n\n{sentence}\n\nZeitpunkt: {}{ip_line}\n\n\
             Warst du das? Dann ist alles in Ordnung.\n\n\
             Wenn nicht: Melde dich an, ändere dein Passwort und schau dir deine Anmeldungen an:\n\
             https://{hostname}/account/security\n\
             Sag außerdem der Person Bescheid, die deinen Mailserver betreut.\n\n\
             UwUMail auf {hostname}\n",
            when(now)
        ),
        Language::En => format!(
            "Hi {name},\n\n{sentence}\n\nTime: {}{ip_line}\n\n\
             Was this you? Then everything is fine.\n\n\
             If not: log in, change your password and check where you are logged in:\n\
             https://{hostname}/account/security\n\
             Also tell the person who runs your mail server.\n\n\
             UwUMail on {hostname}\n",
            when(now)
        ),
    };
    let domain = account.login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(hostname);
    let message = MessageBuilder::new()
        .from(("UwUMail".to_owned(), format!("postmaster@{domain}")))
        .to((name.to_owned(), account.login.clone()))
        .subject(subject)
        .date(Date::new(now))
        .message_id(format!("{}.security@{hostname}", hex_id()))
        .header("Auto-Submitted", mail_builder::headers::text::Text::new("auto-generated"))
        .text_body(body)
        .write_to_vec();
    let Ok(raw) = message else {
        tracing::error!(login = %account.login, "building a security notice failed");
        return;
    };
    // A service has no mailbox of its own; its notice goes where its mail goes, and nowhere
    // when it takes none.
    let Ok(Some(account_id)) = web.store().delivery_target(account.id).await else {
        tracing::info!(login = %account.login, "no mailbox for a security notice");
        return;
    };
    let request = IngestRequest {
        account_id,
        raw,
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    if let Err(err) = web.store().ingest(request).await {
        tracing::warn!(%err, login = %account.login, "delivering a security notice failed");
    }
}

/// German letters go on in lower case after "Hallo Leni,"; English ones start a new sentence.
fn sentence_case(sentence: String) -> String {
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => sentence,
    }
}

fn hex_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system RNG failed");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_short_and_in_utc() {
        assert_eq!(when(1_789_483_000), "2026-09-15 14:36 UTC");
        assert_eq!(sentence_case("the password".into()), "The password");
    }
}
