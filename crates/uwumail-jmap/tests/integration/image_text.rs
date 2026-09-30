//! `Email/imageText`: the text in a message's pictures, read by a stand-in for Tesseract
//! (docs/jmap-image-text.md).

use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use uwumail_assist::PictureRead;
use uwumail_jmap::ocr::OcrConfig;

use crate::common::{USING, server_with};

const IMAGETEXT: &str = "urn:uwumail:jmap:imagetext";

fn using() -> Vec<&'static str> {
    let mut using = USING.to_vec();
    using.push(IMAGETEXT);
    using
}

fn png(width: u32, height: u32) -> String {
    let mut out = Vec::new();
    image::RgbImage::new(width, height).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    BASE64.encode(out)
}

/// A poster embedded in the HTML, a picture attached, an icon too small to read and a remote picture.
fn message() -> String {
    format!(
        "From: kino@example.com\nTo: mini@example.org\nSubject: Premiere\nMessage-ID: <poster@example.com>\n\
         MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=outer\n\n\
         --outer\nContent-Type: multipart/related; boundary=inner\n\n\
         --inner\nContent-Type: text/html; charset=utf-8\n\n\
         <p>Komm vorbei!</p><img src=\"cid:poster@example.com\"><img src=\"cid:icon@example.com\">\
         <img src=\"http://127.0.0.1/banner.png\" width=\"600\">\n\
         --inner\nContent-Type: image/png\nContent-ID: <poster@example.com>\nContent-Transfer-Encoding: base64\n\n{}\n\
         --inner\nContent-Type: image/png\nContent-ID: <icon@example.com>\nContent-Transfer-Encoding: base64\n\n{}\n\
         --inner--\n\
         --outer\nContent-Type: image/png; name=flyer.png\nContent-Disposition: attachment; filename=flyer.png\n\
         Content-Transfer-Encoding: base64\n\n{}\n\
         --outer--\n",
        png(320, 180),
        png(16, 16),
        png(200, 300),
    )
}

/// Answers `--version`, and "Premiere am Freitag" for every picture.
fn fake_tesseract(dir: &std::path::Path) -> OcrConfig {
    let script = dir.join("tesseract");
    std::fs::write(
        &script,
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\ncat > /dev/null\necho 'Premiere am Freitag'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    OcrConfig { command: script.display().to_string(), ..OcrConfig::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_text_in_a_message_s_pictures_is_read() {
    let tools = tempfile::tempdir().unwrap();
    let config = fake_tesseract(tools.path());
    let server = server_with(|jmap| jmap.with_ocr(config)).await;
    let session = server.session_of("mini@example.org").await;
    assert_eq!(session["capabilities"][IMAGETEXT]["unavailable"], false);
    assert_eq!(session["capabilities"][IMAGETEXT]["maxImages"], 20);
    let account = server.account_id("mini@example.org").await;
    let email = server.deliver("mini@example.org", &message()).await;

    let call =
        |remote: Value| json!([["Email/imageText", { "accountId": account, "emailId": email, "remote": remote }, "0"]]);
    let responses = server.api_using("mini@example.org", &using(), call(json!(false))).await;
    let result = &responses[0][1];
    assert_eq!(responses[0][0], "Email/imageText", "{result}");
    assert_eq!(result["accountId"], account.as_str());
    assert_eq!(result["emailId"], email.as_str());
    assert_eq!(result["unavailable"], false);
    let images = result["images"].as_array().unwrap();
    assert_eq!(images.len(), 2, "{result}");
    assert_eq!(images[0]["source"], "cid:poster@example.com");
    assert_eq!(images[0]["text"], "Premiere am Freitag");
    assert_eq!((images[0]["width"].as_u64(), images[0]["height"].as_u64()), (Some(320), Some(180)));
    let attachment = images[1]["source"].as_str().unwrap();
    assert!(attachment.starts_with("blob:p"), "{attachment}");
    assert_eq!(images[1]["width"], 200);
    assert_eq!(result["skipped"], 1, "the icon is too small to read");

    // The remote picture only when asked for; this one is not allowed (it is inside), so skipped too.
    let responses = server.api_using("mini@example.org", &using(), call(json!(true))).await;
    assert_eq!(responses[0][1]["images"].as_array().unwrap().len(), 2);
    assert_eq!(responses[0][1]["skipped"], 2);

    // Not for someone else's message, and not without the capability.
    let responses = server
        .api_using(
            "mini@example.org",
            &using(),
            json!([["Email/imageText", { "accountId": account, "emailId": "e999999" }, "0"]]),
        )
        .await;
    assert_eq!(responses[0][1]["type"], "notFound");
    let responses = server.api("mini@example.org", call(json!(false))).await;
    assert_eq!(responses[0][1]["type"], "unknownMethod");
    let responses = server.api_using("mini@example.org", &using(), call(json!("yes"))).await;
    assert_eq!(responses[0][1]["type"], "invalidArguments");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_tesseract_the_server_says_so() {
    let missing = OcrConfig { command: "/nonexistent/tesseract".into(), ..OcrConfig::default() };
    let server = server_with(|jmap| jmap.with_ocr(missing)).await;
    assert_eq!(server.session_of("mini@example.org").await["capabilities"][IMAGETEXT]["unavailable"], true);
    let account = server.account_id("mini@example.org").await;
    let email = server.deliver("mini@example.org", &message()).await;
    let responses = server
        .api_using(
            "mini@example.org",
            &using(),
            json!([["Email/imageText", { "accountId": account, "emailId": email, "remote": false }, "0"]]),
        )
        .await;
    assert_eq!(
        responses[0][1],
        json!({ "accountId": account, "emailId": email, "unavailable": true, "images": [], "skipped": 0 })
    );
    // The default server without a configuration reads nothing either unless Tesseract is there.
    let plain = crate::common::server().await;
    assert!(plain.session_of("mini@example.org").await["capabilities"][IMAGETEXT].is_object());
}

/// The AI assistant's `includeImages` reads the same text, from the message's own pictures only.
#[tokio::test(flavor = "multi_thread")]
async fn the_assistant_reads_the_same_text() {
    let tools = tempfile::tempdir().unwrap();
    let config = fake_tesseract(tools.path());
    let server = server_with(|jmap| jmap.with_ocr(config)).await;
    let email = server.deliver("mini@example.org", &message()).await;
    let email: i64 = email.trim_start_matches('e').parse().unwrap();
    let account = server.id("mini@example.org").await;
    let read = server.jmap.image_text_reader();
    // `Assist/estimate` only looks at what was read before, and reads nothing itself.
    let known = read(account, email, PictureRead::KnownOnly).await.unwrap();
    assert_eq!((known.texts.len(), known.unread), (0, 2));
    let texts = read(account, email, PictureRead::Read).await.unwrap().texts;
    assert_eq!(texts, ["Premiere am Freitag", "Premiere am Freitag"]);
    let known = read(account, email, PictureRead::KnownOnly).await.unwrap();
    assert_eq!((known.texts, known.unread), (texts, 0));
    let stranger = server.id("nyu@example.org").await;
    assert_eq!(read(stranger, email, PictureRead::Read).await, None, "not someone else's message");

    let missing = OcrConfig { command: "/nonexistent/tesseract".into(), ..OcrConfig::default() };
    let server = server_with(|jmap| jmap.with_ocr(missing)).await;
    let email = server.deliver("mini@example.org", &message()).await;
    let email: i64 = email.trim_start_matches('e').parse().unwrap();
    let account = server.id("mini@example.org").await;
    assert_eq!(server.jmap.image_text_reader()(account, email, PictureRead::Read).await, None);
}
