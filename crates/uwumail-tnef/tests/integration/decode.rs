use uwumail_tnef::{HtmlSource, Value, decode, mapi};

use crate::fixtures;

#[test]
fn a_note_with_attachments() {
    let message = decode(&fixtures::note()).unwrap();
    assert!(message.complete);
    assert_eq!(message.message_class.as_deref(), Some("IPM.Note"));
    assert_eq!(message.subject.as_deref(), Some("Umzug"));
    assert_eq!(message.sent_at, Some(fixtures::START));
    let sender = message.sender.as_ref().unwrap();
    assert_eq!(sender.email.as_deref(), Some("mini@example.com"), "the SMTP address, not the Exchange one");
    assert_eq!(sender.name.as_deref(), Some("Mini"));

    let html = message.body.html.as_deref().unwrap();
    assert_eq!(message.body.html_source, Some(HtmlSource::RtfEncapsulated));
    assert!(html.contains("<p>Hallo Nyu, anbei der Bericht über den Umzug."), "{html}");
    assert!(html.contains(r#"<img src="cid:logo@example.com">"#), "{html}");
    assert_eq!(message.body.text.as_deref(), Some("Hallo Nyu, anbei der Bericht über den Umzug."));
    assert!(message.body.rtf.as_deref().unwrap().starts_with(b"{\\rtf1"));

    assert_eq!(message.attachments.len(), 3);
    let pdf = &message.attachments[0];
    assert_eq!(pdf.name.as_deref(), Some("Quartalsbericht 2026 – Übersicht.pdf"));
    assert_eq!(pdf.mime_type, "application/pdf");
    assert_eq!(pdf.data, b"%PDF-1.7 fake");
    assert!(!pdf.inline);

    let logo = &message.attachments[1];
    assert_eq!(logo.content_id.as_deref(), Some("logo@example.com"));
    assert_eq!(logo.mime_type, "image/png");
    assert!(logo.inline && logo.hidden);

    let forwarded = &message.attachments[2];
    assert_eq!(forwarded.mime_type, "message/rfc822");
    let inner = forwarded.embedded.as_ref().unwrap();
    assert_eq!(inner.subject.as_deref(), Some("Weitergeleitet"));
    assert_eq!(inner.body.text.as_deref(), Some("Innen"));
    assert_eq!(inner.attachments[0].data, b"inside");
    assert_eq!(inner.attachments[0].mime_type, "text/plain");
}

#[test]
fn legacy_attributes_and_code_pages() {
    let mut t = uwumail_tnef::builder::Tnef::new();
    t.code_page(1251);
    t.message_class("IPM.Note");
    // attSubject and attBody in the stream's code page.
    t.attribute(1, 0x0001_8004, b"\xcf\xf0\xe8\xe2\xe5\xf2\0");
    t.attribute(1, 0x0002_800C, b"\xd2\xe5\xea\xf1\xf2");
    t.message_props(&uwumail_tnef::builder::Props::new().string8(0x3001, b"\xc8\xec\xff"));
    let message = decode(&t.build()).unwrap();
    assert_eq!(message.code_page, 1251);
    assert_eq!(message.subject.as_deref(), Some("Привет"));
    assert_eq!(message.body.text.as_deref(), Some("Текст"));
    assert_eq!(message.properties.tag(0x3001), Some(&Value::String("Имя".into())));
}

#[test]
fn html_body_and_real_rtf() {
    let props = uwumail_tnef::builder::Props::new()
        .binary(mapi::PR_HTML, b"<html><body><p>Gr\xfc\xdfe</p></body></html>")
        .long(mapi::PR_INTERNET_CPID, 1252);
    let message = decode(&fixtures::simple("IPM.Note", props)).unwrap();
    assert_eq!(message.body.html.as_deref(), Some("<html><body><p>Grüße</p></body></html>"));
    assert_eq!(message.body.html_source, Some(HtmlSource::Html));
    assert_eq!(message.body.text.as_deref(), Some("Grüße"));

    let rtf = br"{\rtf1\ansi{\fonttbl{\f0 Calibri;}}\pard Erste {\b Zeile}\par Zweite\par}";
    let props = uwumail_tnef::builder::Props::new()
        .binary(mapi::PR_RTF_COMPRESSED, &uwumail_tnef::builder::compressed_rtf(rtf));
    let message = decode(&fixtures::simple("IPM.Note", props)).unwrap();
    assert_eq!(message.body.text.as_deref(), Some("Erste Zeile\nZweite"));
    assert_eq!(message.body.html.as_deref(), Some("<div>Erste <b>Zeile</b></div>\n<div>Zweite</div>\n"));
    assert_eq!(message.body.html_source, Some(HtmlSource::Rtf));
}

#[test]
fn a_cut_stream_keeps_what_came_before() {
    let full = fixtures::note();
    let message = decode(&full[..full.len() - 40]).unwrap();
    assert!(!message.complete);
    assert_eq!(message.subject.as_deref(), Some("Umzug"));
    assert_eq!(message.attachments.len(), 2);
}

#[test]
fn limits_hold() {
    let limits = uwumail_tnef::Limits { max_attachments: 1, max_depth: 0, ..Default::default() };
    let message = uwumail_tnef::decode_with(&fixtures::note(), &limits).unwrap();
    assert_eq!(message.attachments.len(), 1);
    assert!(!message.complete);
    let limits = uwumail_tnef::Limits { max_depth: 0, ..Default::default() };
    let message = uwumail_tnef::decode_with(&fixtures::note(), &limits).unwrap();
    assert!(message.attachments[2].embedded.is_none(), "attached messages are not decoded below the depth");
    let limits = uwumail_tnef::Limits { max_body: 10, ..Default::default() };
    let message = uwumail_tnef::decode_with(&fixtures::note(), &limits).unwrap();
    assert!(message.body.html.is_none(), "RTF that unpacks to more than allowed is left out");
}
