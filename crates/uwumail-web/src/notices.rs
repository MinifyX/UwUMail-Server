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
    SecondFactorLocked,
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
            Notice::SecondFactorLocked => "secondFactorLocked",
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
                Notice::SecondFactorLocked => (
                    "Anmeldung vorübergehend gesperrt",
                    format!("bei der Anmeldung in dein Konto {login} wurde zu oft ein falscher zweiter Faktor eingegeben, nach dem richtigen Passwort. Die Anmeldung ist deshalb für 15 Minuten gesperrt."),
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
                Notice::SecondFactorLocked => (
                    "Logging in paused",
                    format!("a wrong second factor was entered too often while logging in to your account {login}, after the right password. Logging in is paused for 15 minutes."),
                ),
            },
            Language::Fr => match self {
                Notice::PasswordChanged => {
                    ("Votre mot de passe a été modifié", format!("le mot de passe de votre compte {login} a été modifié."))
                }
                Notice::PasswordChosenWithLink => (
                    "Nouveau mot de passe via un lien",
                    format!("un nouveau mot de passe a été choisi pour votre compte {login} via un lien à usage unique."),
                ),
                Notice::PasswordSetByAdmin => (
                    "Votre mot de passe a été réinitialisé",
                    format!("{actor} a défini un nouveau mot de passe pour votre compte {login}."),
                ),
                Notice::TotpEnabled => (
                    "Application d'authentification activée",
                    "la connexion avec une application d'authentification a été activée. Les applications mail ont désormais besoin de mots de passe d'application."
                        .into(),
                ),
                Notice::TotpDisabled => (
                    "Application d'authentification désactivée",
                    "la connexion avec une application d'authentification a été désactivée.".into(),
                ),
                Notice::PasskeyAdded { name } => {
                    ("Nouvelle clé d'accès", format!("la clé d'accès « {name} » a été ajoutée à votre compte."))
                }
                Notice::PasskeyRemoved { name } => {
                    ("Clé d'accès supprimée", format!("la clé d'accès « {name} » a été supprimée de votre compte."))
                }
                Notice::RecoveryCodesCreated => (
                    "Nouveaux codes de récupération",
                    "de nouveaux codes de récupération ont été créés pour votre compte. Les anciens ne sont plus valables.".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "Code de récupération utilisé",
                    format!("un code de récupération a été utilisé pour se connecter. Il en reste {left}."),
                ),
                Notice::AppPasswordCreated { name } => (
                    "Nouveau mot de passe d'application",
                    format!("le mot de passe d'application « {name} » a été créé pour votre compte."),
                ),
                Notice::AppsMayUseMainPassword => (
                    "Les applications mail peuvent utiliser le mot de passe principal",
                    "les applications mail peuvent de nouveau se connecter avec votre mot de passe principal, et pas seulement avec des mots de passe d'application.".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "Nouveau transfert",
                    format!("un transfert vers {address} a été configuré pour votre compte. Les adresses sur d'autres serveurs doivent encore le confirmer."),
                ),
                Notice::SecondFactorsReset => (
                    "Connexion à deux facteurs réinitialisée",
                    format!("{actor} a réinitialisé la connexion à deux facteurs de votre compte. Vous vous connectez désormais uniquement avec votre mot de passe."),
                ),
                Notice::SecondFactorLocked => (
                    "Connexion temporairement bloquée",
                    format!("un second facteur erroné a été saisi trop souvent lors de la connexion à votre compte {login}, après le bon mot de passe. La connexion est donc bloquée pendant 15 minutes."),
                ),
            },
            Language::Nl => match self {
                Notice::PasswordChanged => {
                    ("Je wachtwoord is gewijzigd", format!("het wachtwoord van je account {login} is gewijzigd."))
                }
                Notice::PasswordChosenWithLink => (
                    "Nieuw wachtwoord via een link",
                    format!("voor je account {login} is via een eenmalige link een nieuw wachtwoord gekozen."),
                ),
                Notice::PasswordSetByAdmin => (
                    "Je wachtwoord is opnieuw ingesteld",
                    format!("{actor} heeft een nieuw wachtwoord ingesteld voor je account {login}."),
                ),
                Notice::TotpEnabled => (
                    "Authenticator-app ingeschakeld",
                    "inloggen met een authenticator-app is ingeschakeld. Mail-apps hebben vanaf nu app-wachtwoorden nodig.".into(),
                ),
                Notice::TotpDisabled => {
                    ("Authenticator-app uitgeschakeld", "inloggen met een authenticator-app is uitgeschakeld.".into())
                }
                Notice::PasskeyAdded { name } => {
                    ("Nieuwe passkey", format!("de passkey ‘{name}’ is aan je account toegevoegd."))
                }
                Notice::PasskeyRemoved { name } => {
                    ("Passkey verwijderd", format!("de passkey ‘{name}’ is van je account verwijderd."))
                }
                Notice::RecoveryCodesCreated => (
                    "Nieuwe herstelcodes",
                    "voor je account zijn nieuwe herstelcodes aangemaakt. De oude werken niet meer.".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "Herstelcode gebruikt",
                    format!("er is een herstelcode gebruikt om in te loggen. Er zijn er nog {left} over."),
                ),
                Notice::AppPasswordCreated { name } => {
                    ("Nieuw app-wachtwoord", format!("voor je account is het app-wachtwoord ‘{name}’ aangemaakt."))
                }
                Notice::AppsMayUseMainPassword => (
                    "Mail-apps mogen het hoofdwachtwoord gebruiken",
                    "mail-apps kunnen weer inloggen met je hoofdwachtwoord, niet alleen met app-wachtwoorden.".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "Nieuwe doorsturing",
                    format!("voor je account is doorsturen naar {address} ingesteld. Adressen op andere servers moeten het nog bevestigen."),
                ),
                Notice::SecondFactorsReset => (
                    "Tweestapsaanmelding teruggezet",
                    format!("{actor} heeft de tweestapsaanmelding van je account teruggezet. Je logt nu alleen nog in met je wachtwoord."),
                ),
                Notice::SecondFactorLocked => (
                    "Inloggen tijdelijk geblokkeerd",
                    format!("bij het inloggen op je account {login} is na het juiste wachtwoord te vaak een verkeerde tweede factor ingevoerd. Inloggen is daarom 15 minuten geblokkeerd."),
                ),
            },
            Language::Ja => match self {
                Notice::PasswordChanged => {
                    ("パスワードが変更されました", format!("アカウント {login} のパスワードが変更されました。"))
                }
                Notice::PasswordChosenWithLink => (
                    "リンクから新しいパスワードが設定されました",
                    format!("アカウント {login} の新しいパスワードが、1回限りのリンクから設定されました。"),
                ),
                Notice::PasswordSetByAdmin => (
                    "パスワードが再設定されました",
                    format!("{actor} がアカウント {login} に新しいパスワードを設定しました。"),
                ),
                Notice::TotpEnabled => (
                    "認証アプリが有効になりました",
                    "認証アプリでのログインが有効になりました。今後、メールアプリにはアプリパスワードが必要です。".into(),
                ),
                Notice::TotpDisabled => {
                    ("認証アプリが無効になりました", "認証アプリでのログインが無効になりました。".into())
                }
                Notice::PasskeyAdded { name } => {
                    ("新しいパスキー", format!("パスキー「{name}」がアカウントに追加されました。"))
                }
                Notice::PasskeyRemoved { name } => {
                    ("パスキーが削除されました", format!("パスキー「{name}」がアカウントから削除されました。"))
                }
                Notice::RecoveryCodesCreated => (
                    "新しい復旧コード",
                    "アカウントの新しい復旧コードが作成されました。以前のコードは使えなくなりました。".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "復旧コードが使用されました",
                    format!("ログインに復旧コードが使用されました。残りは {left} 個です。"),
                ),
                Notice::AppPasswordCreated { name } => (
                    "新しいアプリパスワード",
                    format!("アプリパスワード「{name}」がアカウントに作成されました。"),
                ),
                Notice::AppsMayUseMainPassword => (
                    "メールアプリがメインのパスワードを使えるようになりました",
                    "メールアプリは、アプリパスワードだけでなく、メインのパスワードでも再びログインできます。".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "新しい転送",
                    format!("アカウントに {address} への転送が設定されました。他のサーバーのアドレスでは、まだ確認が必要です。"),
                ),
                Notice::SecondFactorsReset => (
                    "2段階認証がリセットされました",
                    format!("{actor} がアカウントの2段階認証をリセットしました。今後はパスワードだけでログインします。"),
                ),
                Notice::SecondFactorLocked => (
                    "ログインが一時的にロックされました",
                    format!("アカウント {login} へのログインで、正しいパスワードの後に誤った2段階目の認証が何度も入力されました。そのため、ログインは15分間ロックされています。"),
                ),
            },
            Language::Zh => match self {
                Notice::PasswordChanged => {
                    ("你的密码已更改", format!("你的账户 {login} 的密码已被更改。"))
                }
                Notice::PasswordChosenWithLink => (
                    "通过链接设置了新密码",
                    format!("你的账户 {login} 通过一次性链接设置了新密码。"),
                ),
                Notice::PasswordSetByAdmin => {
                    ("你的密码已被重置", format!("{actor} 为你的账户 {login} 设置了新密码。"))
                }
                Notice::TotpEnabled => (
                    "身份验证器应用已开启",
                    "使用身份验证器应用登录已开启。从现在起，邮件应用需要使用应用专用密码。".into(),
                ),
                Notice::TotpDisabled => {
                    ("身份验证器应用已关闭", "使用身份验证器应用登录已关闭。".into())
                }
                Notice::PasskeyAdded { name } => ("新的通行密钥", format!("通行密钥“{name}”已添加到你的账户。")),
                Notice::PasskeyRemoved { name } => {
                    ("通行密钥已移除", format!("通行密钥“{name}”已从你的账户中移除。"))
                }
                Notice::RecoveryCodesCreated => (
                    "新的恢复码",
                    "已为你的账户生成新的恢复码。旧的恢复码不再有效。".into(),
                ),
                Notice::RecoveryCodeUsed { left } => (
                    "已使用恢复码",
                    format!("有人使用恢复码登录。还剩 {left} 个。"),
                ),
                Notice::AppPasswordCreated { name } => {
                    ("新的应用专用密码", format!("已为你的账户创建应用专用密码“{name}”。"))
                }
                Notice::AppsMayUseMainPassword => (
                    "邮件应用可以使用主密码",
                    "邮件应用又可以用你的主密码登录了，而不仅仅是应用专用密码。".into(),
                ),
                Notice::ForwardingAdded { address } => (
                    "新的转发",
                    format!("已为你的账户设置转发到 {address}。其他服务器上的地址还需要确认。"),
                ),
                Notice::SecondFactorsReset => (
                    "两步登录已重置",
                    format!("{actor} 重置了你账户的两步登录。现在你只用密码登录。"),
                ),
                Notice::SecondFactorLocked => (
                    "登录已暂时锁定",
                    format!("登录你的账户 {login} 时，在输入正确密码后，第二步验证多次输入错误。因此登录已被锁定 15 分钟。"),
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

/// The person's own language, or the server's when they left it to their browser.
pub(crate) fn language(web: &Web, preferences: &serde_json::Map<String, Value>) -> Language {
    Language::preferred(preferences.get("language").and_then(Value::as_str), web.smtp().tone().language)
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
    // German letters go on in lower case after "Hallo …,"; the others start a new sentence.
    let sentence = if language == Language::De { sentence } else { sentence_case(sentence) };
    let brand = web.smtp().brand();
    let brand = brand.name();
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
             {brand} auf {hostname}\n",
            when(now)
        ),
        Language::En => format!(
            "Hi {name},\n\n{sentence}\n\nTime: {}{ip_line}\n\n\
             Was this you? Then everything is fine.\n\n\
             If not: log in, change your password and check where you are logged in:\n\
             https://{hostname}/account/security\n\
             Also tell the person who runs your mail server.\n\n\
             {brand} on {hostname}\n",
            when(now)
        ),
        Language::Fr => format!(
            "Bonjour {name},\n\n{sentence}\n\nDate : {}{ip_line}\n\n\
             C'était vous ? Alors tout va bien.\n\n\
             Sinon : connectez-vous, changez votre mot de passe et vérifiez où vous êtes connecté :\n\
             https://{hostname}/account/security\n\
             Prévenez aussi la personne qui gère votre serveur mail.\n\n\
             {brand} sur {hostname}\n",
            when(now)
        ),
        Language::Nl => format!(
            "Hallo {name},\n\n{sentence}\n\nTijdstip: {}{ip_line}\n\n\
             Was jij dit? Dan is alles in orde.\n\n\
             Zo niet: log in, wijzig je wachtwoord en bekijk waar je bent ingelogd:\n\
             https://{hostname}/account/security\n\
             Laat het ook weten aan degene die je mailserver beheert.\n\n\
             {brand} op {hostname}\n",
            when(now)
        ),
        Language::Ja => format!(
            "{name} さん\n\n{sentence}\n\n日時：{}{ip_line}\n\n\
             ご自身の操作であれば、問題ありません。\n\n\
             心当たりがない場合は、ログインしてパスワードを変更し、ログイン中の端末を確認してください：\n\
             https://{hostname}/account/security\n\
             メールサーバーの管理者にもお知らせください。\n\n\
             {brand}（{hostname}）\n",
            when(now)
        ),
        Language::Zh => format!(
            "{name}，你好：\n\n{sentence}\n\n时间：{}{ip_line}\n\n\
             如果是你本人操作，那就没问题。\n\n\
             如果不是：请登录，修改密码，并检查你在哪些地方登录着：\n\
             https://{hostname}/account/security\n\
             另外请告诉管理你邮件服务器的人。\n\n\
             {brand}（{hostname}）\n",
            when(now)
        ),
    };
    let domain = account.login.rsplit_once('@').map(|(_, domain)| domain).unwrap_or(hostname);
    let message = MessageBuilder::new()
        .from((brand.to_owned(), format!("postmaster@{domain}")))
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
