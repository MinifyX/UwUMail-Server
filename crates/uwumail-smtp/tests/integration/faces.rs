//! The `Face:` header (docs/profile-pictures.md): added to a person's own mail when they asked for
//! it, signed with the rest, never on other addresses; kept from incoming mail only when a signature
//! of the From domain covers it.

use lettre::AsyncTransport;
use uwumail_smtp::profile_pictures::{face_header, prepare, sample};
use uwumail_store::{
    DomainMaskedPolicy, MaskedMode, MaskedState, NewMaskedAddress, NewPicture, PictureVisibility, ProfileUpdate,
};

use crate::flow::{PASSWORD, RawSession, TestServer, mail, start};

/// Gives `login` a public picture and switches the Face header on.
async fn with_face(server: &TestServer, login: &str) -> Vec<u8> {
    let store = server.smtp.store();
    let account = store.account(login).await.unwrap().unwrap().id;
    let prepared = prepare(&sample(120, 120, "png")).unwrap();
    let update = ProfileUpdate {
        picture: Some(Some(NewPicture {
            bytes: prepared.bytes,
            media_type: prepared.media_type.into(),
            face: Some(prepared.face.clone()),
        })),
        visibility: Some(PictureVisibility::Public),
        send_face: Some(true),
    };
    store.update_profile(account, update).await.unwrap();
    prepared.face
}

fn face_of(raw: &str) -> Option<String> {
    let start = raw.find("\r\nFace:")? + 2;
    let block = &raw[start..raw.find("\r\n\r\n")?];
    let mut value = String::new();
    for (index, line) in block.split("\r\n").enumerate() {
        if index > 0 && !line.starts_with([' ', '\t']) {
            break;
        }
        value.push_str(line.trim_start_matches("Face:").trim());
    }
    Some(value)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_face_goes_out_signed_and_is_kept_when_dmarc_passes() {
    let b = start("b.test", &["nyu"], &[]).await;
    let a = start("a.test", &["mini", "ami"], &[("b.test", b.mx)]).await;
    for key in uwumail_smtp::dkim::ensure_domain_keys(a.smtp.store(), "a.test").await.unwrap() {
        let (name, value) = key.dns_record();
        b.smtp.dns_cache().pin_txt(&name, &value).unwrap();
    }
    for name in ["a.test", "mx.a.test"] {
        b.smtp.dns_cache().pin_no_txt(name);
    }
    b.smtp.dns_cache().pin_txt("_dmarc.a.test", "v=DMARC1; p=none").unwrap();
    let face = with_face(&a, "mini@a.test").await;
    let expected = face_of(&format!("\r\n{}\r\n", face_header(&face))).unwrap();

    a.mailer("mini@a.test", PASSWORD, false)
        .send(mail("Mini <mini@a.test>", &["nyu@b.test", "ami@a.test"], "Mit Gesicht"))
        .await
        .unwrap();
    let local = a.raw(&a.wait_for_inbox("ami@a.test", 1).await[0]).await;
    assert_eq!(face_of(&local), Some(expected.clone()), "{local}");
    let remote = b.raw(&b.wait_for_inbox("nyu@b.test", 1).await[0]).await;
    assert_eq!(face_of(&remote), Some(expected), "{remote}");
    assert_eq!(remote.matches("dkim=pass").count(), 2, "the Face is signed with the rest: {remote}");
    assert!(remote.contains("dmarc=pass"), "{remote}");

    // b.test keeps it for pictureUrl.
    let kept = b.smtp.store().received_face("mini@a.test").await.unwrap().expect("kept");
    assert_eq!(kept.0, face);
    // The signature names the Face, so a receiver can tell that a.test sent it.
    let unfolded = remote.replace("\r\n\t", "").replace("\r\n ", "");
    let signature = unfolded.lines().find(|line| line.starts_with("DKIM-Signature:")).unwrap();
    let signed = signature.split(';').map(str::trim).find_map(|tag| tag.strip_prefix("h=")).unwrap();
    assert!(signed.split(':').any(|name| name.trim().eq_ignore_ascii_case("Face")), "{signature}");
    // Our own addresses keep no Face: people here choose themselves who sees their picture.
    assert!(a.smtp.store().received_face("mini@a.test").await.unwrap().is_none());

    // The same signed mail sent again with another picture in its Face is not believed.
    let original = face_header(&face);
    let swapped = remote.replace(original.trim_end(), face_header(&sample(48, 48, "png")).trim_end());
    assert_ne!(swapped, remote);
    let mut session = RawSession::connect(b.mx).await;
    assert!(session.command("EHLO mx.a.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<nyu@b.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let reply = session.command(&format!("{}\r\n.", swapped.trim_end_matches("\r\n"))).await;
    assert!(reply.starts_with("250"), "{reply}");
    b.wait_for_inbox("nyu@b.test", 2).await;
    assert_eq!(b.smtp.store().received_face("mini@a.test").await.unwrap().unwrap().0, face);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_persons_own_addresses_carry_a_face() {
    let a = start("a.test", &["mini", "ami"], &[]).await;
    let store = a.smtp.store();
    with_face(&a, "mini@a.test").await;
    store.add_alias("hallo@a.test", "mini@a.test").await.unwrap();
    store
        .set_domain_masked_policy("a.test", DomainMaskedPolicy { mode: MaskedMode::Own, ..Default::default() })
        .await
        .unwrap();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let masked = store
        .create_masked_address(
            mini,
            NewMaskedAddress {
                domain: None,
                state: Some(MaskedState::Enabled),
                for_domain: "https://shop.example.com".into(),
                description: String::new(),
                url: None,
                email_prefix: None,
                created_by: "test".into(),
            },
        )
        .await
        .unwrap();

    let mailer = a.mailer("mini@a.test", PASSWORD, false);
    mailer.send(mail("hallo@a.test", &["ami@a.test"], "Alias")).await.unwrap();
    mailer.send(mail(&masked.email, &["ami@a.test"], "Maskiert")).await.unwrap();
    let inbox = a.wait_for_inbox("ami@a.test", 2).await;
    for email in &inbox {
        let raw = a.raw(email).await;
        match email.subject.as_str() {
            "Alias" => assert!(face_of(&raw).is_some(), "an alias is the person's own: {raw}"),
            _ => assert!(face_of(&raw).is_none(), "a masked address gives nothing away: {raw}"),
        }
    }

    // A Face the mail app wrote goes, whoever sends: the server decides which picture is shown.
    let written = format!(
        "From: ami@a.test\r\nTo: mini@a.test\r\nSubject: Eigenes Gesicht\r\n{}\r\nHallo\r\n",
        face_header(&sample(48, 48, "png"))
    );
    let envelope =
        lettre::address::Envelope::new(Some("ami@a.test".parse().unwrap()), vec!["mini@a.test".parse().unwrap()])
            .unwrap();
    a.mailer("ami@a.test", PASSWORD, false).send_raw(&envelope, written.as_bytes()).await.unwrap();
    let raw = a.raw(&a.wait_for_inbox("mini@a.test", 1).await[0]).await;
    assert!(face_of(&raw).is_none(), "{raw}");

    // Switched off, even the person's own mail goes without.
    store.update_profile(mini, ProfileUpdate { send_face: Some(false), ..Default::default() }).await.unwrap();
    mailer.send(mail("mini@a.test", &["ami@a.test"], "Ohne")).await.unwrap();
    let inbox = a.wait_for_inbox("ami@a.test", 3).await;
    let without = inbox.iter().find(|email| email.subject == "Ohne").unwrap();
    assert!(face_of(&a.raw(without).await).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn incoming_faces_need_a_signature_over_them() {
    let a = start("a.test", &["mini"], &[]).await;
    a.smtp.dns_cache().pin_txt("sender.test", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    a.smtp.dns_cache().pin_no_txt("mail.sender.test");
    a.smtp.dns_cache().pin_txt("_dmarc.sender.test", "v=DMARC1; p=none").unwrap();
    let mut session = RawSession::connect(a.mx).await;
    assert!(session.command("EHLO mail.sender.test").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<leni@sender.test>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    let face = face_header(&sample(48, 48, "png"));
    let reply = session
        .command(&format!("From: leni@sender.test\r\nSubject: Hallo\r\n{}\r\n\r\nHallo\r\n.", face.trim_end()))
        .await;
    assert!(reply.starts_with("250"), "{reply}");

    // SPF passes aligned and the domain publishes DMARC, so the mail is the sender's. But SPF says
    // nothing about the headers: anyone could have written the Face on the way. Not kept.
    assert_eq!(a.inbox("mini@a.test").await.len(), 1);
    assert!(a.smtp.store().received_face("leni@sender.test").await.unwrap().is_none());
}
