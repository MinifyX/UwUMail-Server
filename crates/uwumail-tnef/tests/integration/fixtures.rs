//! TNEF streams like the ones Outlook writes, made with the crate's own writer.

use uwumail_tnef::builder::{self, Pattern, Props, Tnef};
use uwumail_tnef::mapi::{self, IID_IMESSAGE, PSETID_APPOINTMENT, PSETID_MEETING};

/// 2026-10-27 09:00 UTC, a Tuesday.
pub const START: i64 = 1_793_091_600;

pub const HTML_RTF: &[u8] = br#"{\rtf1\ansi\ansicpg1252\fromhtml1 \deff0{\fonttbl{\f0\fswiss Arial;}}
{\*\htmltag19 <html>}{\*\htmltag50 <body>}
{\*\htmltag64 <p>}\htmlrtf {\htmlrtf0 Hallo Nyu, anbei der Bericht \'fcber den Umzug.\htmlrtf\par}\htmlrtf0
{\*\htmltag84 <img src="cid:logo@example.com">}
{\*\htmltag72 </p>}{\*\htmltag58 </body>}{\*\htmltag27 </html>}}"#;

/// A note with a body only in compressed RTF, a PDF with a long Unicode name, an inline picture
/// and an attached message.
pub fn note() -> Vec<u8> {
    let inner = {
        let mut t = Tnef::new();
        t.message_class("IPM.Note");
        t.message_props(&Props::new().unicode(mapi::PR_SUBJECT, "Weitergeleitet").unicode(mapi::PR_BODY, "Innen"));
        t.attachment("inner.txt", b"inside", &Props::new());
        t.build()
    };
    let mut t = Tnef::new();
    t.code_page(1252);
    t.message_class("IPM.Note");
    t.message_props(
        &Props::new()
            .unicode(mapi::PR_SUBJECT, "Umzug")
            .unicode(mapi::PR_SENT_REPRESENTING_NAME, "Mini")
            .unicode(mapi::PR_SENT_REPRESENTING_ADDRTYPE, "EX")
            .unicode(mapi::PR_SENT_REPRESENTING_EMAIL_ADDRESS, "/O=EXCHANGELABS/OU=X/CN=RECIPIENTS/CN=MINI")
            .unicode(mapi::PR_SENT_REPRESENTING_SMTP_ADDRESS, "mini@example.com")
            .time(mapi::PR_CLIENT_SUBMIT_TIME, START)
            .binary(mapi::PR_RTF_COMPRESSED, &builder::compressed_rtf(HTML_RTF)),
    );
    t.attachment(
        "QUARTA~1.PDF",
        b"%PDF-1.7 fake",
        &Props::new()
            .unicode(mapi::PR_ATTACH_LONG_FILENAME, "Quartalsbericht 2026 – Übersicht.pdf")
            .long(mapi::PR_ATTACH_METHOD, 1),
    );
    t.attachment(
        "image001.png",
        b"\x89PNG\r\n\x1a\nfake",
        &Props::new()
            .unicode(mapi::PR_ATTACH_MIME_TAG, "image/png")
            .unicode(mapi::PR_ATTACH_CONTENT_ID, "<logo@example.com>")
            .bool(mapi::PR_ATTACHMENT_HIDDEN, true),
    );
    t.attachment(
        "Weitergeleitet",
        &[],
        &Props::new().unicode(mapi::PR_DISPLAY_NAME, "Weitergeleitet").long(mapi::PR_ATTACH_METHOD, 5).object(
            mapi::PR_ATTACH_DATA,
            &IID_IMESSAGE,
            &inner,
        ),
    );
    t.build()
}

pub fn organizer_props(class_subject: &str) -> Props {
    Props::new()
        .unicode(mapi::PR_SUBJECT, class_subject)
        .unicode(mapi::PR_CONVERSATION_TOPIC, "Umzugsplanung")
        .unicode(mapi::PR_SENT_REPRESENTING_NAME, "Mini Organizer")
        .unicode(mapi::PR_SENT_REPRESENTING_SMTP_ADDRESS, "mini@example.com")
        .unicode(mapi::PR_BODY, "Wir planen den Umzug.\r\nBitte Kisten mitbringen; danke, Mini")
        .time(mapi::PR_CLIENT_SUBMIT_TIME, START - 86_400)
        .named_time(&PSETID_APPOINTMENT, 0x820D, START)
        .named_time(&PSETID_APPOINTMENT, 0x820E, START + 3600)
        .named_unicode(&PSETID_APPOINTMENT, 0x8208, "Raum 1, Etage 2")
        .named_long(&PSETID_APPOINTMENT, 0x8201, 3)
        .named_long(&PSETID_APPOINTMENT, 0x8205, 2)
        .named_binary(&PSETID_MEETING, 0x0003, &builder::global_object_id("umzug-2026@example.com", None))
        .named_binary(
            &PSETID_APPOINTMENT,
            0x825E,
            &builder::time_zone_definition("W. Europe Standard Time", -60, -60, Some(((10, 5, 0, 3), (3, 5, 0, 2)))),
        )
}

fn recipient(name: &str, email: &str, kind: i32) -> Props {
    Props::new()
        .unicode(mapi::PR_DISPLAY_NAME, name)
        .unicode(mapi::PR_ADDRTYPE, "SMTP")
        .unicode(mapi::PR_EMAIL_ADDRESS, email)
        .long(mapi::PR_RECIPIENT_TYPE, kind)
}

/// A weekly meeting on Tuesdays and Thursdays, ten times, one instance taken out.
pub fn request() -> Vec<u8> {
    let recurrence = builder::recurrence(&Pattern {
        frequency: 0x200B,
        pattern_type: 1,
        period: 1,
        specific: vec![0b0001_0100],
        end_type: 0x2022,
        occurrences: 10,
        first_weekday: 1,
        deleted: vec![builder::minutes_1601(2026, 11, 3)],
        modified: vec![],
        start_date: builder::minutes_1601(2026, 10, 27),
        end_date: builder::minutes_1601(2026, 11, 26),
        start_offset: 10 * 60,
        end_offset: 11 * 60,
    });
    let mut t = Tnef::new();
    t.message_class("IPM.Schedule.Meeting.Request");
    t.message_props(&organizer_props("Umzugsplanung").named_bool(&PSETID_APPOINTMENT, 0x8223, true).named_binary(
        &PSETID_APPOINTMENT,
        0x8216,
        &recurrence,
    ));
    t.recipients(&[
        recipient("Nyu", "nyu@example.com", 1),
        recipient("Ami", "ami@example.com", 2),
        recipient("Raum 1", "raum1@example.com", 3),
        recipient("Mini Organizer", "mini@example.com", 1),
    ]);
    t.build()
}

pub fn simple(class: &str, props: Props) -> Vec<u8> {
    let mut t = Tnef::new();
    t.message_class(class);
    t.message_props(&props);
    t.build()
}
