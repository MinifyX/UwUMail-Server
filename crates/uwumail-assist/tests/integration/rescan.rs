//! The one-time look for codes after an update (docs/verification-codes.md).

use tokio::sync::watch;

use crate::common::rig;

fn mail(block: &str, n: u32) -> String {
    format!(
        "{block}From: Lock <noreply@lock.example.com>\nTo: Mia <mia@example.org>\nSubject: Anmeldung\n\
Message-ID: <code-{n}@lock.example.com>\nContent-Type: text/plain; charset=utf-8\n\n\
Dein Anmeldecode: 133900\nEr gilt 10 Minuten.\n"
    )
}

fn ours(results: &str) -> String {
    format!(
        "Received: from lock.example.com\n\tby mx.example.org (UwUMail) with ESMTPSA id 1\n\
Authentication-Results: mx.example.org; {results}\n"
    )
}

#[tokio::test]
async fn codes_are_looked_for_once_in_mail_with_our_verdicts() {
    let rig = rig().await;
    let signed = rig.deliver(&rig.mia, &mail(&ours("dkim=pass header.d=example.com; dmarc=none"), 1)).await;
    let spf_only =
        rig.deliver(&rig.mia, &mail(&ours("spf=pass smtp.mailfrom=bounce@example.com; dmarc=none"), 2)).await;
    // A DKIM pass for another domain, a copy of our block in an appended mail, and no block at all.
    let stranger = rig.deliver(&rig.mia, &mail(&ours("dkim=pass header.d=attacker.example; dmarc=none"), 3)).await;
    let appended = rig.append(&rig.mia, &mail(&ours("dkim=pass header.d=example.com; dmarc=pass"), 4)).await;
    let bare = rig.deliver(&rig.mia, &mail("", 5)).await;

    let (_tx, shutdown) = watch::channel(false);
    rig.assist.clone().run_verification_rescan("0.0.0-test", shutdown.clone()).await;
    let found = rig.store.verifications(rig.mia.id, vec![signed, spf_only, stranger, appended, bare]).await.unwrap();
    assert_eq!(found[&signed].code.as_deref(), Some("133900"));
    assert!(found[&signed].autofill);
    assert!(!found[&spf_only].autofill, "SPF alone is shown, never filled in");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(rig.store.setting("verification-rescan:0.0.0-test").await.unwrap().is_some());

    // Done once: a mail that arrives later without its code is not read again by this version.
    let later = rig.deliver(&rig.mia, &mail(&ours("dkim=pass header.d=example.com; dmarc=pass"), 6)).await;
    rig.assist.clone().run_verification_rescan("0.0.0-test", shutdown).await;
    assert!(rig.store.verifications(rig.mia.id, vec![later]).await.unwrap().is_empty());
}
