//! The words of alert mails to admins, per language and tone. Only the greeting and the
//! sign-off change with the tone; what is wrong is said the same way in both.

use uwumail_smtp::{InternalTone, Language};
use uwumail_store::{AlertEvent, AlertLevel};

/// One line's label: how bad it is, or that it is fine again.
pub(crate) fn label(language: Language, event: AlertEvent, level: AlertLevel) -> &'static str {
    use Language as L;
    let problem = level == AlertLevel::Problem;
    match (language, event, problem) {
        (L::De, AlertEvent::Resolved, _) => "Wieder in Ordnung",
        (L::De, AlertEvent::Reminder, _) => "Immer noch ein Problem",
        (L::De, _, true) => "Problem",
        (L::De, _, false) => "Bitte ansehen",
        (L::En, AlertEvent::Resolved, _) => "Fine again",
        (L::En, AlertEvent::Reminder, _) => "Still a problem",
        (L::En, _, true) => "Problem",
        (L::En, _, false) => "Needs a look",
        (L::Fr, AlertEvent::Resolved, _) => "De nouveau en ordre",
        (L::Fr, AlertEvent::Reminder, _) => "Toujours un problème",
        (L::Fr, _, true) => "Problème",
        (L::Fr, _, false) => "À vérifier",
        (L::Nl, AlertEvent::Resolved, _) => "Weer in orde",
        (L::Nl, AlertEvent::Reminder, _) => "Nog steeds een probleem",
        (L::Nl, _, true) => "Probleem",
        (L::Nl, _, false) => "Even kijken",
        (L::Ja, AlertEvent::Resolved, _) => "解決しました",
        (L::Ja, AlertEvent::Reminder, _) => "問題が続いています",
        (L::Ja, _, true) => "問題",
        (L::Ja, _, false) => "要確認",
        (L::Zh, AlertEvent::Resolved, _) => "已恢复正常",
        (L::Zh, AlertEvent::Reminder, _) => "问题仍未解决",
        (L::Zh, _, true) => "问题",
        (L::Zh, _, false) => "需要查看",
    }
}

/// What is wrong, in one sentence. `{domain}` stands for the domain it is about.
pub(crate) fn finding(language: Language, code: &str) -> &'static str {
    match language {
        Language::De => match code {
            "noDomains" => "Es ist noch keine Domain eingerichtet, deshalb kann keine Mail ankommen.",
            "tlsFailures" => "Andere Server melden gescheiterte verschlüsselte Verbindungen zu {domain}.",
            "dmarcOwnFailures" => "Mail von diesem Server hat DMARC für {domain} nicht bestanden.",
            "dnsDomain" => "DNS-Einträge von {domain} fehlen oder sind falsch.",
            "certMissing" => "Es ist kein Zertifikat geladen.",
            "certWaiting" => "Das Let's-Encrypt-Zertifikat ist noch nicht da.",
            "certSelfSigned" => "Der Server nutzt ein selbst signiertes Zertifikat.",
            "certWrongName" => "Das Zertifikat passt nicht zum Hostnamen.",
            "certExpired" => "Das Zertifikat ist abgelaufen.",
            "certExpiresSoon" => "Das Zertifikat läuft bald ab.",
            "certRenewalFailing" => "Das Zertifikat lässt sich seit über einem Tag nicht erneuern.",
            "gatewayRefused" => "Das UwUMail Gateway nimmt diesen Server nicht an.",
            "gatewayDown" => "Es gibt keine Verbindung zum UwUMail Gateway.",
            "relayLogin" => "Das Relay lehnt die Anmeldung ab.",
            "relayUnreachable" => "Das Relay ist nicht erreichbar.",
            "relayTls" => "Zum Relay gibt es keine verschlüsselte Verbindung.",
            "outboundBlocked" => "Der Server erreicht keine anderen Mailserver.",
            "gatewayOutboundBlocked" => "Über das Gateway sind keine anderen Mailserver erreichbar.",
            "probeDns" => "Die Versandprüfung konnte keine Namen auflösen.",
            "gatewayPort25Blocked" => "Das Gateway erreicht andere Mailserver nicht über Port 25.",
            "port25Blocked" => "Ausgehender Port 25 scheint gesperrt zu sein.",
            "queueStuck" => "In der Warteschlange hängt Mail fest.",
            "manyBounces" => "Viele Nachrichten wurden von anderen Servern abgelehnt.",
            "virusScannerAway" => "Der Virenscanner antwortet nicht.",
            "virusSignaturesOld" => "Die Virensignaturen sind veraltet.",
            "diskLow" => "Der Speicherplatz wird knapp.",
            "mailboxesNearlyFull" => "Postfächer sind fast voll.",
            "adminsWithoutSecondFactor" => "Ein Admin meldet sich ohne zweiten Faktor an.",
            "backupFailed" => "Das letzte Backup ist fehlgeschlagen.",
            "backupOld" => "Das letzte erfolgreiche Backup ist älter als zwei Tage.",
            "microsoftBlocked" => "Microsoft (Outlook, Hotmail) lehnt Mail von {ip} ab ({code}).",
            "microsoftThrottled" => "Microsoft (Outlook, Hotmail) drosselt Mail von {ip} ({code}).",
            "microsoftAuth" => {
                "Microsoft lehnt Mail von {domain} ab, weil SPF, DKIM oder DMARC nicht reichen ({code})."
            }
            _ => "Etwas am Server braucht deine Aufmerksamkeit.",
        },
        Language::En => match code {
            "noDomains" => "No domain is set up yet, so no mail can arrive.",
            "tlsFailures" => "Other servers report failed encrypted connections to {domain}.",
            "dmarcOwnFailures" => "Mail from this server failed DMARC for {domain}.",
            "dnsDomain" => "DNS records of {domain} are missing or wrong.",
            "certMissing" => "No certificate is loaded.",
            "certWaiting" => "The Let's Encrypt certificate has not arrived yet.",
            "certSelfSigned" => "The server uses a self-signed certificate.",
            "certWrongName" => "The certificate does not match the host name.",
            "certExpired" => "The certificate has expired.",
            "certExpiresSoon" => "The certificate expires soon.",
            "certRenewalFailing" => "Renewing the certificate has been failing for more than a day.",
            "gatewayRefused" => "The UwUMail Gateway does not accept this server.",
            "gatewayDown" => "There is no connection to the UwUMail Gateway.",
            "relayLogin" => "The relay refuses the login.",
            "relayUnreachable" => "The relay cannot be reached.",
            "relayTls" => "There is no encrypted connection to the relay.",
            "outboundBlocked" => "The server cannot reach other mail servers.",
            "gatewayOutboundBlocked" => "Other mail servers cannot be reached through the gateway.",
            "probeDns" => "The sending check could not resolve names.",
            "gatewayPort25Blocked" => "The gateway cannot reach other mail servers on port 25.",
            "port25Blocked" => "Outgoing port 25 seems to be blocked.",
            "queueStuck" => "Mail is stuck in the queue.",
            "manyBounces" => "Many messages were refused by other servers.",
            "virusScannerAway" => "The virus scanner does not answer.",
            "virusSignaturesOld" => "The virus signatures are out of date.",
            "diskLow" => "Disk space is running low.",
            "mailboxesNearlyFull" => "Mailboxes are nearly full.",
            "adminsWithoutSecondFactor" => "An admin logs in without a second factor.",
            "backupFailed" => "The last backup failed.",
            "backupOld" => "The last successful backup is more than two days old.",
            "microsoftBlocked" => "Microsoft (Outlook, Hotmail) refuses mail from {ip} ({code}).",
            "microsoftThrottled" => "Microsoft (Outlook, Hotmail) throttles mail from {ip} ({code}).",
            "microsoftAuth" => {
                "Microsoft refuses mail from {domain} because SPF, DKIM or DMARC are not good enough ({code})."
            }
            _ => "Something about the server needs your attention.",
        },
        Language::Fr => match code {
            "noDomains" => "Aucun domaine n'est encore configuré, aucun mail ne peut donc arriver.",
            "tlsFailures" => "D'autres serveurs signalent des connexions chiffrées échouées vers {domain}.",
            "dmarcOwnFailures" => "Des mails de ce serveur ont échoué à DMARC pour {domain}.",
            "dnsDomain" => "Des enregistrements DNS de {domain} manquent ou sont incorrects.",
            "certMissing" => "Aucun certificat n'est chargé.",
            "certWaiting" => "Le certificat Let's Encrypt n'est pas encore arrivé.",
            "certSelfSigned" => "Le serveur utilise un certificat auto-signé.",
            "certWrongName" => "Le certificat ne correspond pas au nom d'hôte.",
            "certExpired" => "Le certificat a expiré.",
            "certExpiresSoon" => "Le certificat expire bientôt.",
            "certRenewalFailing" => "Le renouvellement du certificat échoue depuis plus d'un jour.",
            "gatewayRefused" => "La passerelle UwUMail n'accepte pas ce serveur.",
            "gatewayDown" => "Il n'y a aucune connexion à la passerelle UwUMail.",
            "relayLogin" => "Le relais refuse la connexion.",
            "relayUnreachable" => "Le relais est injoignable.",
            "relayTls" => "Il n'y a pas de connexion chiffrée vers le relais.",
            "outboundBlocked" => "Le serveur ne peut pas joindre d'autres serveurs mail.",
            "gatewayOutboundBlocked" => "Les autres serveurs mail sont injoignables via la passerelle.",
            "probeDns" => "Le test d'envoi n'a pas pu résoudre les noms.",
            "gatewayPort25Blocked" => "La passerelle ne peut pas joindre d'autres serveurs mail sur le port 25.",
            "port25Blocked" => "Le port 25 sortant semble bloqué.",
            "queueStuck" => "Des mails sont bloqués dans la file d'attente.",
            "manyBounces" => "De nombreux messages ont été refusés par d'autres serveurs.",
            "virusScannerAway" => "L'antivirus ne répond pas.",
            "virusSignaturesOld" => "Les signatures antivirus sont obsolètes.",
            "diskLow" => "L'espace disque devient insuffisant.",
            "mailboxesNearlyFull" => "Des boîtes mail sont presque pleines.",
            "adminsWithoutSecondFactor" => "Un admin se connecte sans second facteur.",
            "backupFailed" => "La dernière sauvegarde a échoué.",
            "backupOld" => "La dernière sauvegarde réussie date de plus de deux jours.",
            "microsoftBlocked" => "Microsoft (Outlook, Hotmail) refuse le courrier de {ip} ({code}).",
            "microsoftThrottled" => "Microsoft (Outlook, Hotmail) limite le courrier de {ip} ({code}).",
            "microsoftAuth" => {
                "Microsoft refuse le courrier de {domain}, car SPF, DKIM ou DMARC ne suffisent pas ({code})."
            }
            _ => "Quelque chose sur le serveur demande votre attention.",
        },
        Language::Nl => match code {
            "noDomains" => "Er is nog geen domein ingesteld, dus er kan geen mail aankomen.",
            "tlsFailures" => "Andere servers melden mislukte versleutelde verbindingen met {domain}.",
            "dmarcOwnFailures" => "Mail van deze server is voor {domain} niet door DMARC gekomen.",
            "dnsDomain" => "DNS-records van {domain} ontbreken of kloppen niet.",
            "certMissing" => "Er is geen certificaat geladen.",
            "certWaiting" => "Het Let's Encrypt-certificaat is er nog niet.",
            "certSelfSigned" => "De server gebruikt een zelfondertekend certificaat.",
            "certWrongName" => "Het certificaat past niet bij de hostnaam.",
            "certExpired" => "Het certificaat is verlopen.",
            "certExpiresSoon" => "Het certificaat verloopt binnenkort.",
            "certRenewalFailing" => "Het certificaat vernieuwen mislukt al meer dan een dag.",
            "gatewayRefused" => "De UwUMail Gateway accepteert deze server niet.",
            "gatewayDown" => "Er is geen verbinding met de UwUMail Gateway.",
            "relayLogin" => "De relay weigert de aanmelding.",
            "relayUnreachable" => "De relay is niet bereikbaar.",
            "relayTls" => "Er is geen versleutelde verbinding met de relay.",
            "outboundBlocked" => "De server kan geen andere mailservers bereiken.",
            "gatewayOutboundBlocked" => "Via de gateway zijn geen andere mailservers bereikbaar.",
            "probeDns" => "De verzendcontrole kon geen namen opzoeken.",
            "gatewayPort25Blocked" => "De gateway bereikt andere mailservers niet via poort 25.",
            "port25Blocked" => "Uitgaande poort 25 lijkt geblokkeerd.",
            "queueStuck" => "Er zit mail vast in de wachtrij.",
            "manyBounces" => "Veel berichten zijn door andere servers geweigerd.",
            "virusScannerAway" => "De virusscanner antwoordt niet.",
            "virusSignaturesOld" => "De virussignaturen zijn verouderd.",
            "diskLow" => "De schijfruimte raakt op.",
            "mailboxesNearlyFull" => "Postvakken zijn bijna vol.",
            "adminsWithoutSecondFactor" => "Een admin logt in zonder tweede factor.",
            "backupFailed" => "De laatste back-up is mislukt.",
            "backupOld" => "De laatste geslaagde back-up is meer dan twee dagen oud.",
            "microsoftBlocked" => "Microsoft (Outlook, Hotmail) weigert mail van {ip} ({code}).",
            "microsoftThrottled" => "Microsoft (Outlook, Hotmail) remt mail van {ip} af ({code}).",
            "microsoftAuth" => "Microsoft weigert mail van {domain}, omdat SPF, DKIM of DMARC niet voldoen ({code}).",
            _ => "Er is iets met de server dat je aandacht nodig heeft.",
        },
        Language::Ja => match code {
            "noDomains" => "ドメインがまだ設定されていないため、メールを受信できません。",
            "tlsFailures" => "他のサーバーから、{domain} への暗号化接続の失敗が報告されています。",
            "dmarcOwnFailures" => "このサーバーからのメールが {domain} の DMARC に合格しませんでした。",
            "dnsDomain" => "{domain} の DNS レコードが不足しているか、間違っています。",
            "certMissing" => "証明書が読み込まれていません。",
            "certWaiting" => "Let's Encrypt の証明書がまだ届いていません。",
            "certSelfSigned" => "サーバーは自己署名証明書を使っています。",
            "certWrongName" => "証明書がホスト名と一致しません。",
            "certExpired" => "証明書の有効期限が切れています。",
            "certExpiresSoon" => "証明書の有効期限がまもなく切れます。",
            "certRenewalFailing" => "証明書の更新が1日以上失敗し続けています。",
            "gatewayRefused" => "UwUMail Gateway がこのサーバーを受け入れていません。",
            "gatewayDown" => "UwUMail Gateway に接続できていません。",
            "relayLogin" => "リレーがログインを拒否しています。",
            "relayUnreachable" => "リレーに接続できません。",
            "relayTls" => "リレーへの暗号化接続がありません。",
            "outboundBlocked" => "サーバーが他のメールサーバーに接続できません。",
            "gatewayOutboundBlocked" => "ゲートウェイ経由で他のメールサーバーに接続できません。",
            "probeDns" => "送信チェックで名前を解決できませんでした。",
            "gatewayPort25Blocked" => "ゲートウェイがポート 25 で他のメールサーバーに接続できません。",
            "port25Blocked" => "送信用のポート 25 がブロックされているようです。",
            "queueStuck" => "キューにメールが滞留しています。",
            "manyBounces" => "多くのメッセージが他のサーバーに拒否されました。",
            "virusScannerAway" => "ウイルススキャナーが応答しません。",
            "virusSignaturesOld" => "ウイルス定義が古くなっています。",
            "diskLow" => "ディスクの空き容量が少なくなっています。",
            "mailboxesNearlyFull" => "メールボックスがいっぱいになりかけています。",
            "adminsWithoutSecondFactor" => "2段階認証なしでログインしている管理者がいます。",
            "backupFailed" => "前回のバックアップが失敗しました。",
            "backupOld" => "最後に成功したバックアップから2日以上経っています。",
            "microsoftBlocked" => "Microsoft（Outlook、Hotmail）が {ip} からのメールを拒否しています（{code}）。",
            "microsoftThrottled" => "Microsoft（Outlook、Hotmail）が {ip} からのメールを制限しています（{code}）。",
            "microsoftAuth" => {
                "SPF、DKIM または DMARC が不十分なため、Microsoft が {domain} からのメールを拒否しています（{code}）。"
            }
            _ => "サーバーについて確認が必要なことがあります。",
        },
        Language::Zh => match code {
            "noDomains" => "还没有设置任何域名，因此无法接收邮件。",
            "tlsFailures" => "其他服务器报告到 {domain} 的加密连接失败。",
            "dmarcOwnFailures" => "本服务器发出的邮件未通过 {domain} 的 DMARC 检查。",
            "dnsDomain" => "{domain} 的 DNS 记录缺失或有误。",
            "certMissing" => "没有加载证书。",
            "certWaiting" => "Let's Encrypt 证书还没有到。",
            "certSelfSigned" => "服务器使用的是自签名证书。",
            "certWrongName" => "证书与主机名不匹配。",
            "certExpired" => "证书已过期。",
            "certExpiresSoon" => "证书即将过期。",
            "certRenewalFailing" => "证书续期已经失败超过一天。",
            "gatewayRefused" => "UwUMail Gateway 不接受此服务器。",
            "gatewayDown" => "与 UwUMail Gateway 的连接已断开。",
            "relayLogin" => "中继服务器拒绝登录。",
            "relayUnreachable" => "无法连接到中继服务器。",
            "relayTls" => "与中继服务器之间没有加密连接。",
            "outboundBlocked" => "服务器无法连接到其他邮件服务器。",
            "gatewayOutboundBlocked" => "通过网关无法连接到其他邮件服务器。",
            "probeDns" => "发信检查无法解析域名。",
            "gatewayPort25Blocked" => "网关无法通过 25 端口连接其他邮件服务器。",
            "port25Blocked" => "出站 25 端口似乎被封锁了。",
            "queueStuck" => "有邮件卡在队列中。",
            "manyBounces" => "许多邮件被其他服务器拒收。",
            "virusScannerAway" => "病毒扫描程序没有响应。",
            "virusSignaturesOld" => "病毒库已过时。",
            "diskLow" => "磁盘空间不足。",
            "mailboxesNearlyFull" => "有邮箱快满了。",
            "adminsWithoutSecondFactor" => "有管理员登录时没有使用第二重验证。",
            "backupFailed" => "上一次备份失败了。",
            "backupOld" => "最近一次成功的备份已超过两天。",
            "microsoftBlocked" => "Microsoft（Outlook、Hotmail）拒收来自 {ip} 的邮件（{code}）。",
            "microsoftThrottled" => "Microsoft（Outlook、Hotmail）正在限制来自 {ip} 的邮件（{code}）。",
            "microsoftAuth" => "由于 SPF、DKIM 或 DMARC 不符合要求，Microsoft 拒收来自 {domain} 的邮件（{code}）。",
            _ => "服务器有需要你注意的地方。",
        },
    }
}

/// Whether the mail is about something wrong, or only about things that are fine again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mood {
    Problem,
    Warning,
    Fine,
}

pub(crate) fn subject(language: Language, tone: InternalTone, mood: Mood, hostname: &str) -> String {
    use {InternalTone as T, Language as L, Mood as M};
    let text = match (language, mood) {
        (L::De, M::Problem) => format!("Problem auf {hostname}"),
        (L::De, M::Warning) => format!("Auf {hostname} braucht etwas einen Blick"),
        (L::De, M::Fine) => format!("Auf {hostname} ist wieder alles in Ordnung"),
        (L::En, M::Problem) => format!("Problem on {hostname}"),
        (L::En, M::Warning) => format!("Something on {hostname} needs a look"),
        (L::En, M::Fine) => format!("{hostname} is fine again"),
        (L::Fr, M::Problem) => format!("Problème sur {hostname}"),
        (L::Fr, M::Warning) => format!("Quelque chose sur {hostname} est à vérifier"),
        (L::Fr, M::Fine) => format!("Tout est de nouveau en ordre sur {hostname}"),
        (L::Nl, M::Problem) => format!("Probleem op {hostname}"),
        (L::Nl, M::Warning) => format!("Op {hostname} moet iets bekeken worden"),
        (L::Nl, M::Fine) => format!("{hostname} is weer in orde"),
        (L::Ja, M::Problem) => format!("{hostname} で問題が発生しています"),
        (L::Ja, M::Warning) => format!("{hostname} で確認が必要なことがあります"),
        (L::Ja, M::Fine) => format!("{hostname} は元どおりになりました"),
        (L::Zh, M::Problem) => format!("{hostname} 出现问题"),
        (L::Zh, M::Warning) => format!("{hostname} 有需要查看的地方"),
        (L::Zh, M::Fine) => format!("{hostname} 已恢复正常"),
    };
    let face = match mood {
        M::Problem => " (╥﹏╥)",
        M::Warning => " (・_・;)",
        M::Fine => " (=^･ω･^=)",
    };
    if tone == T::Playful { text + face } else { text }
}

/// The whole mail around the lines in `items`.
pub(crate) struct Letter<'a> {
    pub language: Language,
    pub tone: InternalTone,
    pub mood: Mood,
    pub name: &'a str,
    pub hostname: &'a str,
    pub brand: &'a str,
    pub items: &'a str,
}

pub(crate) fn body(letter: Letter<'_>) -> String {
    use {InternalTone as T, Language as L, Mood as M};
    let Letter { language, tone, mood, name, hostname, brand, items } = letter;
    let playful = tone == T::Playful;
    let fine = mood == M::Fine;
    let admin = format!("https://{hostname}/admin");
    match language {
        L::De => {
            let intro = match (playful, fine) {
                (true, true) => format!("gute Nachrichten von {hostname}, alles schnurrt wieder (=^･ω･^=)"),
                (true, false) => format!("ich hab auf {hostname} nach dem Rechten gesehen und etwas gefunden (・_・;)"),
                (false, true) => format!("auf {hostname} ist wieder in Ordnung, was vorher nicht ging:"),
                (false, false) => format!("auf {hostname} hat sich etwas geändert, das du wissen solltest:"),
            };
            let sign =
                if playful { format!("Nyu von {brand} auf {hostname} ♡") } else { format!("{brand} auf {hostname}") };
            format!(
                "Hallo {name},\n\n{intro}\n\n{items}\nWas zu tun ist, steht im Admin-Bereich: {admin}\n\n\
                 Du bekommst diese Mails als Admin dieses Servers. Welche (alle, nur Probleme, keine), \
                 stellst du auf der Server-Übersicht ein.\n\n{sign}\n"
            )
        }
        L::En => {
            let intro = match (playful, fine) {
                (true, true) => format!("Good news from {hostname}, everything purrs again (=^･ω･^=)"),
                (true, false) => format!("I kept an eye on {hostname} and found something (・_・;)"),
                (false, true) => format!("On {hostname}, what was wrong is fine again:"),
                (false, false) => format!("On {hostname}, something changed that you should know about:"),
            };
            let sign =
                if playful { format!("Nyu from {brand} on {hostname} ♡") } else { format!("{brand} on {hostname}") };
            format!(
                "Hi {name},\n\n{intro}\n\n{items}\nThe admin panel shows what to do: {admin}\n\n\
                 You get these mails as an admin of this server. Which ones (all, only problems, none) \
                 you choose on the server overview.\n\n{sign}\n"
            )
        }
        L::Fr => {
            let intro = match (playful, fine) {
                (true, true) => format!("Bonne nouvelle de {hostname}, tout ronronne de nouveau (=^･ω･^=)"),
                (true, false) => format!("J'ai jeté un œil à {hostname} et j'ai trouvé quelque chose (・_・;)"),
                (false, true) => format!("Sur {hostname}, ce qui n'allait pas est de nouveau en ordre :"),
                (false, false) => format!("Sur {hostname}, quelque chose a changé que vous devriez savoir :"),
            };
            let sign =
                if playful { format!("Nyu de {brand} sur {hostname} ♡") } else { format!("{brand} sur {hostname}") };
            format!(
                "Bonjour {name},\n\n{intro}\n\n{items}\nL'espace d'administration indique quoi faire : {admin}\n\n\
                 Vous recevez ces mails en tant qu'admin de ce serveur. Lesquels (tous, seulement les problèmes, \
                 aucun), vous le choisissez sur la vue d'ensemble du serveur.\n\n{sign}\n"
            )
        }
        L::Nl => {
            let intro = match (playful, fine) {
                (true, true) => format!("Goed nieuws van {hostname}, alles spint weer (=^･ω･^=)"),
                (true, false) => format!("Ik heb op {hostname} rondgekeken en iets gevonden (・_・;)"),
                (false, true) => format!("Op {hostname} is weer in orde wat eerder misging:"),
                (false, false) => format!("Op {hostname} is iets veranderd dat je moet weten:"),
            };
            let sign =
                if playful { format!("Nyu van {brand} op {hostname} ♡") } else { format!("{brand} op {hostname}") };
            format!(
                "Hallo {name},\n\n{intro}\n\n{items}\nIn het beheer staat wat je kunt doen: {admin}\n\n\
                 Je krijgt deze mails als admin van deze server. Welke (alle, alleen problemen, geen) \
                 kies je op het serveroverzicht.\n\n{sign}\n"
            )
        }
        L::Ja => {
            let intro = match (playful, fine) {
                (true, true) => format!("{hostname} からいいお知らせです。ぜんぶ元どおりになりました (=^･ω･^=)"),
                (true, false) => format!("{hostname} の様子を見ていたら、気になることを見つけました (・_・;)"),
                (false, true) => format!("{hostname} で問題になっていたことが解決しました："),
                (false, false) => format!("{hostname} で、お知らせしておきたい変化がありました："),
            };
            let sign = if playful {
                format!("{brand}（{hostname}）のニュウより ♡")
            } else {
                format!("{brand}（{hostname}）")
            };
            format!(
                "{name} さん\n\n{intro}\n\n{items}\n対処方法は管理画面で確認できます：{admin}\n\n\
                 このメールは、このサーバーの管理者に送られています。受け取るメール（すべて・問題のみ・なし）は\
                 サーバーの概要ページで選べます。\n\n{sign}\n"
            )
        }
        L::Zh => {
            let intro = match (playful, fine) {
                (true, true) => format!("{hostname} 传来好消息，一切又恢复正常了 (=^･ω･^=)"),
                (true, false) => format!("我在 {hostname} 上巡视了一圈，发现了一些情况 (・_・;)"),
                (false, true) => format!("{hostname} 上之前的问题已经恢复正常："),
                (false, false) => format!("{hostname} 上发生了一些你需要知道的变化："),
            };
            let sign = if playful {
                format!("{brand}（{hostname}）的 Nyu ♡")
            } else {
                format!("{brand}（{hostname}）")
            };
            format!(
                "{name}，你好：\n\n{intro}\n\n{items}\n在管理后台可以看到该怎么处理：{admin}\n\n\
                 你作为这台服务器的管理员收到这些邮件。要接收哪些邮件（全部、仅问题、不接收），\
                 可以在服务器概览页面上选择。\n\n{sign}\n"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: &[&str] = &[
        "microsoftBlocked",
        "microsoftThrottled",
        "microsoftAuth",
        "noDomains",
        "tlsFailures",
        "dmarcOwnFailures",
        "dnsDomain",
        "certMissing",
        "certWaiting",
        "certSelfSigned",
        "certWrongName",
        "certExpired",
        "certExpiresSoon",
        "certRenewalFailing",
        "gatewayRefused",
        "gatewayDown",
        "relayLogin",
        "relayUnreachable",
        "relayTls",
        "outboundBlocked",
        "gatewayOutboundBlocked",
        "probeDns",
        "gatewayPort25Blocked",
        "port25Blocked",
        "queueStuck",
        "manyBounces",
        "virusScannerAway",
        "virusSignaturesOld",
        "diskLow",
        "mailboxesNearlyFull",
        "adminsWithoutSecondFactor",
        "backupFailed",
        "backupOld",
    ];

    #[test]
    fn every_finding_is_said_in_every_language() {
        for language in Language::ALL {
            let fallback = finding(language, "somethingNew");
            for code in CODES {
                let text = finding(language, code);
                assert_ne!(text, fallback, "{code} in {language:?}");
                assert_eq!(
                    text.contains("{domain}"),
                    ["tlsFailures", "dmarcOwnFailures", "dnsDomain", "microsoftAuth"].contains(code)
                );
                assert_eq!(text.contains("{ip}"), ["microsoftBlocked", "microsoftThrottled"].contains(code));
            }
        }
    }

    #[test]
    fn the_tone_changes_greeting_and_subject_only() {
        let letter = |tone| Letter {
            language: Language::En,
            tone,
            mood: Mood::Problem,
            name: "Nyu",
            hostname: "mail.example.org",
            brand: "UwUMail",
            items: "• Problem: Mail is stuck in the queue.\n",
        };
        let playful = body(letter(InternalTone::Playful));
        let neutral = body(letter(InternalTone::Neutral));
        assert!(playful.contains("(・_・;)") && !neutral.contains("(・_・;)"));
        assert!(neutral.contains("Mail is stuck") && neutral.contains("https://mail.example.org/admin"));
        assert_eq!(
            subject(Language::De, InternalTone::Neutral, Mood::Fine, "mail.example.org"),
            "Auf mail.example.org ist wieder alles in Ordnung"
        );
    }
}
