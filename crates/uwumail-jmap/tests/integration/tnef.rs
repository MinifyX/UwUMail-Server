//! winmail.dat: what is inside shows as the mail's own attachments and body, and the TNEF part
//! itself only in `bodyStructure`.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget};
use uwumail_tnef::builder::{Props, Tnef, compressed_rtf, global_object_id, mime_with_winmail};
use uwumail_tnef::mapi::{self, IID_IMESSAGE, PSETID_APPOINTMENT, PSETID_MEETING};

use crate::common::{PASSWORD, Server, args, basic, server};

const START: i64 = 1_793_091_600;

const RTF: &[u8] = br#"{\rtf1\ansi\ansicpg1252\fromhtml1 {\*\htmltag19 <html>}{\*\htmltag50 <body>}
{\*\htmltag64 <p>}\htmlrtf {\htmlrtf0 Hallo Nyu, anbei der Bericht \'fcber den Umzug.\htmlrtf\par}\htmlrtf0
{\*\htmltag84 <img src="cid:logo@example.com">}{\*\htmltag72 </p>}{\*\htmltag58 </body>}{\*\htmltag27 </html>}}"#;

fn note() -> Vec<u8> {
    let inner = {
        let mut t = Tnef::new();
        t.message_props(&Props::new().unicode(mapi::PR_SUBJECT, "Weitergeleitet").unicode(mapi::PR_BODY, "Innen"));
        t.build()
    };
    let mut t = Tnef::new();
    t.message_class("IPM.Note");
    t.message_props(&Props::new().binary(mapi::PR_RTF_COMPRESSED, &compressed_rtf(RTF)));
    t.attachment(
        "QUARTA~1.PDF",
        b"%PDF-1.7 fake",
        &Props::new().unicode(mapi::PR_ATTACH_LONG_FILENAME, "Quartalsbericht 2026 – Übersicht.pdf"),
    );
    t.attachment(
        "image001.png",
        b"\x89PNG\r\n\x1a\nfake",
        &Props::new().unicode(mapi::PR_ATTACH_CONTENT_ID, "logo@example.com").bool(mapi::PR_ATTACHMENT_HIDDEN, true),
    );
    t.attachment(
        "Weitergeleitet",
        &[],
        &Props::new().long(mapi::PR_ATTACH_METHOD, 5).object(mapi::PR_ATTACH_DATA, &IID_IMESSAGE, &inner),
    );
    t.build()
}

fn request() -> Vec<u8> {
    let recipient = Props::new()
        .unicode(mapi::PR_DISPLAY_NAME, "Mini")
        .unicode(mapi::PR_ADDRTYPE, "SMTP")
        .unicode(mapi::PR_EMAIL_ADDRESS, "mini@example.org")
        .long(mapi::PR_RECIPIENT_TYPE, 1);
    let mut t = Tnef::new();
    t.message_class("IPM.Schedule.Meeting.Request");
    t.message_props(
        &Props::new()
            .unicode(mapi::PR_SUBJECT, "Umzugsplanung")
            .unicode(mapi::PR_SENT_REPRESENTING_NAME, "Gast")
            .unicode(mapi::PR_SENT_REPRESENTING_SMTP_ADDRESS, "gast@example.com")
            .unicode(mapi::PR_BODY, "Wir planen den Umzug.")
            .named_time(&PSETID_APPOINTMENT, 0x820D, START)
            .named_time(&PSETID_APPOINTMENT, 0x820E, START + 3600)
            .named_binary(&PSETID_MEETING, 0x0003, &global_object_id("umzug@example.com", None)),
    );
    t.recipients(&[recipient]);
    t.build()
}

async fn deliver(server: &Server, raw: Vec<u8>) -> String {
    let ingested = server
        .store
        .ingest(IngestRequest {
            account_id: server.id("mini@example.org").await,
            raw,
            mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
            keywords: vec![],
            received_at: None,
        })
        .await
        .unwrap();
    format!("e{}", ingested.id)
}

async fn download(server: &Server, account: &str, blob: &str) -> (StatusCode, String, Vec<u8>) {
    let request = Request::get(format!("/jmap/download/{account}/{blob}/file"))
        .header(header::AUTHORIZATION, basic("mini@example.org", PASSWORD))
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(server.router.clone(), request).await.unwrap();
    let status = response.status();
    let kind = response.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap().to_vec();
    (status, kind, bytes)
}

fn names(list: &Value) -> Vec<String> {
    list.as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap_or("").to_owned()).collect()
}

#[tokio::test]
async fn winmail_dat_shows_as_the_mail() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let raw = mime_with_winmail(
        "From: Mini <mini@example.com>\r\nTo: mini@example.org\r\nSubject: Umzug\r\nDate: Mon, 26 Oct 2026 10:00:00 +0100\r\n",
        Some(""),
        &note(),
    );
    let id = deliver(&server, raw).await;
    let responses = server
        .api(
            "mini@example.org",
            json!([
                ["Email/get", { "accountId": account, "ids": [id],
                    "properties": ["textBody", "htmlBody", "attachments", "bodyValues", "bodyStructure", "hasAttachment", "preview", "uwuSafeHtml"],
                    "fetchAllBodyValues": true }, "0"],
                ["Email/query", { "accountId": account, "filter": { "text": "Quartalsbericht" } }, "1"],
            ]),
        )
        .await;
    let email = &args(&responses, 0, "Email/get")["list"][0];
    assert_eq!(args(&responses, 1, "Email/query")["ids"], json!([id]), "attachment names inside are searched");
    assert_eq!(email["hasAttachment"], true);
    assert_eq!(email["preview"], "Hallo Nyu, anbei der Bericht über den Umzug.");

    // The multipart is part 0, the empty text part 1, winmail.dat part 2.
    assert_eq!(email["htmlBody"][0]["partId"], "2.html");
    assert_eq!(email["htmlBody"][0]["type"], "text/html");
    assert_eq!(email["textBody"][0]["partId"], "2.text");
    let html = email["bodyValues"]["2.html"]["value"].as_str().unwrap();
    assert!(html.contains("<p>Hallo Nyu, anbei der Bericht über den Umzug."), "{html}");
    assert_eq!(email["bodyValues"]["2.text"]["value"], "Hallo Nyu, anbei der Bericht über den Umzug.");
    assert!(email["uwuSafeHtml"].as_str().unwrap().contains("Hallo Nyu"));

    let attachments = &email["attachments"];
    assert_eq!(names(attachments), ["Quartalsbericht 2026 – Übersicht.pdf", "image001.png", "Weitergeleitet.eml"]);
    assert_eq!(attachments[0]["type"], "application/pdf");
    assert_eq!(attachments[0]["partId"], "2.1");
    assert_eq!(attachments[0]["disposition"], "attachment");
    assert_eq!(attachments[0]["size"], 13);
    assert_eq!(attachments[1]["cid"], "logo@example.com");
    assert_eq!(attachments[1]["disposition"], "inline");
    assert_eq!(attachments[2]["type"], "message/rfc822");
    // The TNEF part is still in the structure, as it came.
    assert!(email["bodyStructure"].to_string().contains("winmail.dat"));

    let pdf = attachments[0]["blobId"].as_str().unwrap();
    let (status, kind, bytes) = download(&server, &account, pdf).await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("application/pdf"), "{kind}");
    assert_eq!(bytes, b"%PDF-1.7 fake");
    let (status, _, bytes) = download(&server, &account, attachments[2]["blobId"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let eml = String::from_utf8_lossy(&bytes);
    assert!(eml.contains("Subject: Weitergeleitet") && eml.contains("Innen"), "{eml}");
    let (status, _, _) = download(&server, &account, &pdf.replace("_2.1", "_2.9")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A decoded attachment can be sent on, like any other part.
    let drafts = server.mailbox("mini@example.org", "drafts").await;
    let responses = server
        .api(
            "mini@example.org",
            json!([["Email/set", { "accountId": account, "create": { "fwd": {
                "mailboxIds": { drafts: true },
                "subject": "Fwd: Umzug",
                "bodyStructure": { "type": "multipart/mixed", "subParts": [
                    { "partId": "t", "type": "text/plain" },
                    { "blobId": pdf, "type": "application/pdf", "name": "bericht.pdf", "disposition": "attachment" }
                ] },
                "bodyValues": { "t": { "value": "Siehe Anhang" } }
            } } }, "0"]]),
        )
        .await;
    let created = &args(&responses, 0, "Email/set")["created"]["fwd"];
    assert!(created["id"].is_string(), "{}", responses[0]);
}

#[tokio::test]
async fn a_meeting_in_winmail_dat_is_an_invitation() {
    let server = server().await;
    let account = server.account_id("mini@example.org").await;
    let raw = mime_with_winmail(
        "From: Gast <gast@example.com>\r\nTo: Mini <mini@example.org>\r\nSubject: Umzugsplanung\r\n",
        Some("Wir planen den Umzug."),
        &request(),
    );
    let id = deliver(&server, raw).await;
    let responses = server
        .api(
            "mini@example.org",
            json!([["Email/get", { "accountId": account, "ids": [id], "properties": ["attachments", "textBody", "htmlBody"] }, "0"]]),
        )
        .await;
    let email = &args(&responses, 0, "Email/get")["list"][0];
    // The MIME text stays the body; there is no HTML to add.
    assert_eq!(email["textBody"][0]["partId"], "1");
    let attachments = email["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1, "{attachments:?}");
    let invite = &attachments[0];
    assert_eq!(invite["type"], "text/calendar");
    assert_eq!(invite["name"], "invite.ics");
    assert_eq!(invite["partId"], "2.ics");
    let (status, kind, bytes) = download(&server, &account, invite["blobId"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.starts_with("text/calendar"), "{kind}");
    let ics = String::from_utf8(bytes).unwrap().replace("\r\n ", "");
    assert!(ics.contains("METHOD:REQUEST"), "{ics}");
    assert!(ics.contains("UID:umzug@example.com"));
    assert!(ics.contains("ORGANIZER;CN=\"Gast\":mailto:gast@example.com"));
    assert!(ics.contains("mailto:mini@example.org"));
    assert!(ics.contains("DTSTART:20261027T090000Z"));
}
