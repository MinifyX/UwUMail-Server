//! Email/imageText: the text in a message's pictures, read with OCR, under the capability
//! `urn:uwumail:jmap:imagetext` (docs/jmap-image-text.md).
//!
//! Embedded pictures and picture attachments are read always; remote pictures only when asked to
//! (`remote: true`, which a client sends only once the person let this message load its pictures), and
//! then only through the server's cache and egress like any remote picture.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use mail_parser::{MimeHeaders, PartType};
use serde_json::{Value, json};
use uwumail_store::mime_limits::parse_message;

use super::Ctx;
use super::email::visible_records;
use crate::error::{MethodError, MethodResult};
use crate::ids;
use crate::ocr::Skip;

/// Pictures read per message; the rest count as skipped.
pub const MAX_IMAGES: usize = 20;
/// Pictures of one message read at the same time (the server as a whole runs fewer).
const PARALLEL: usize = 2;
/// A call takes at most this long; pictures not read by then count as skipped.
const DEADLINE: Duration = Duration::from_secs(90);
/// Remote pictures looked for in this much of a message's HTML.
const MAX_HTML_SCANNED: usize = 2 * 1024 * 1024;

enum Source {
    Part { source: String, bytes: Vec<u8> },
    Remote(String),
}

pub async fn image_text(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let requested = args
        .get("emailId")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("emailId is required"))?;
    let remote = match args.get("remote") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(remote)) => *remote,
        Some(_) => return Err(MethodError::invalid_arguments("remote is true or false")),
    };
    let id = ctx.parse_id('e', requested).ok_or_else(|| MethodError::kind("notFound"))?;
    let store = &ctx.jmap.store;
    let record = visible_records(ctx, store.emails_by_ids(ctx.account.id, vec![id]).await?)
        .into_iter()
        .next()
        .ok_or_else(|| MethodError::kind("notFound"))?;
    let mut response = json!({
        "accountId": ctx.account_id(),
        "emailId": requested,
        "unavailable": true,
        "images": [],
        "skipped": 0,
    });
    if !ctx.jmap.ocr.available().await {
        return Ok(response);
    }
    let raw = store.blob(&record.blob).await?;
    let (sources, mut skipped) = pictures_of(&raw, &record.blob, remote);

    // The person reading, whose share of the egress remote pictures take.
    let person = ctx.shared.as_ref().map_or(ctx.account.id, |view| view.me.id);
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let ocr = ctx.jmap.ocr.clone();
    let images = ctx.jmap.remote_images.clone();
    let results: Vec<Option<Value>> = futures_util::stream::iter(sources)
        .map(|source| {
            let (ocr, images) = (ocr.clone(), images.clone());
            // The deadline holds for fetching a remote picture too (OCR-1 of the 0.18.0 audit).
            let work = async move {
                let (source, bytes) = match source {
                    Source::Part { source, bytes } => (source, bytes),
                    Source::Remote(url) => {
                        let picture = images.get(person, &url).await.ok()?;
                        (url, picture.bytes.to_vec())
                    }
                };
                let read = ocr.read(&bytes).await;
                match read {
                    Ok(read) if read.text.trim().is_empty() => Some(Value::Null),
                    Ok(read) => Some(json!({
                        "source": source,
                        "text": read.text,
                        "width": read.width,
                        "height": read.height,
                    })),
                    Err(Skip::Unsuitable | Skip::Failed) => None,
                }
            };
            async move { tokio::time::timeout_at(deadline, work).await.ok().flatten() }
        })
        .buffered(PARALLEL)
        .collect()
        .await;
    let mut found = Vec::new();
    for result in results {
        match result {
            // Read, but without any text in it.
            Some(Value::Null) => {}
            Some(image) => found.push(image),
            None => skipped += 1,
        }
    }
    response["unavailable"] = json!(false);
    response["images"] = Value::Array(found);
    response["skipped"] = json!(skipped);
    Ok(response)
}

/// The text in a message's own pictures (embedded ones and picture attachments, never remote ones),
/// for the AI assistant's `Assist/extractEvents` with `includeImages` (docs/llm.md). `None` when
/// nothing can be read here (no Tesseract, no such message); pictures without text are left out.
/// The caller has checked that the message is the person's.
pub(crate) async fn texts_for_assist(
    ocr: &Arc<crate::ocr::Ocr>,
    store: &uwumail_store::Store,
    account_id: i64,
    email_id: i64,
) -> Option<Vec<String>> {
    if !ocr.available().await {
        return None;
    }
    let record = store.emails_by_ids(account_id, vec![email_id]).await.ok()?.into_iter().next()?;
    let raw = store.blob(&record.blob).await.ok()?;
    let (sources, _) = pictures_of(&raw, &record.blob, false);
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let texts: Vec<Option<String>> = futures_util::stream::iter(sources)
        .map(|source| async move {
            let Source::Part { bytes, .. } = source else { return None };
            let read = tokio::time::timeout_at(deadline, ocr.read(&bytes)).await.ok()?.ok()?;
            Some(read.text).filter(|text| !text.trim().is_empty())
        })
        .buffered(PARALLEL)
        .collect()
        .await;
    Some(texts.into_iter().flatten().collect())
}

/// A message's first [`MAX_IMAGES`] pictures in the order they come: embedded ones and attachments,
/// then the remote ones of its HTML when `remote`; and how many more there are.
fn pictures_of(raw: &[u8], blob: &uwumail_store::BlobHash, remote: bool) -> (Vec<Source>, usize) {
    let Some(message) = parse_message(raw) else { return (Vec::new(), 0) };
    let mut found = Vec::new();
    let mut more = 0;
    let mut html = String::new();
    for (index, part) in message.parts.iter().enumerate() {
        match &part.body {
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => {
                let is_image = part.content_type().is_some_and(|kind| kind.ctype().eq_ignore_ascii_case("image"));
                if !is_image {
                    continue;
                }
                if found.len() >= MAX_IMAGES {
                    more += 1;
                    continue;
                }
                let attachment =
                    part.content_disposition().is_some_and(|d| d.ctype().eq_ignore_ascii_case("attachment"));
                let source = match part.content_id().map(|id| id.trim_matches(['<', '>'])) {
                    Some(id) if !attachment && !id.is_empty() => format!("cid:{id}"),
                    _ => format!("blob:{}", ids::part_blob(blob, index)),
                };
                found.push(Source::Part { source, bytes: bytes.to_vec() });
            }
            PartType::Html(text) if remote && html.len() < MAX_HTML_SCANNED => html.push_str(text),
            _ => {}
        }
    }
    if remote {
        let mut seen = std::collections::HashSet::new();
        for url in remote_pictures(&html) {
            if !seen.insert(url.clone()) {
                continue;
            }
            if found.len() >= MAX_IMAGES {
                more += 1;
            } else {
                found.push(Source::Remote(url));
            }
        }
    }
    (found, more)
}

/// The web addresses in the `src` of a piece of HTML's `<img>` tags.
fn remote_pictures(html: &str) -> Vec<String> {
    // Only ASCII changes case, so positions in one are positions in the other.
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<img") {
        let start = from + at + 4;
        let end = lower[start..].find('>').map_or(lower.len(), |end| start + end);
        from = end;
        let tag = &lower[start..end];
        let mut search = 0;
        while let Some(found_at) = tag[search..].find("src") {
            let name = search + found_at;
            search = name + 3;
            if !tag[..name].ends_with(|c: char| c.is_ascii_whitespace() || c == '/') {
                continue;
            }
            // From here on the original, whose case the address keeps.
            let Some(value) = html[start + search..end].trim_start().strip_prefix('=') else { continue };
            let value = value.trim_start();
            let value = match value.chars().next() {
                Some(quote @ ('"' | '\'')) => value[1..].find(quote).map(|close| &value[1..1 + close]),
                _ => value.split(|c: char| c.is_ascii_whitespace()).next(),
            };
            if let Some(value) = value {
                let value = value.trim().replace("&amp;", "&");
                let web = value.get(..8).is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
                    || value.get(..7).is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"));
                if web && value.len() <= 4096 {
                    found.push(value);
                }
            }
            break;
        }
    }
    found
}

/// Whether OCR is there, for the session.
pub async fn capability(ocr: &Arc<crate::ocr::Ocr>) -> Value {
    json!({ "maxImages": MAX_IMAGES, "unavailable": !ocr.available().await })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_pictures_are_found_in_img_tags_only() {
        let html = r#"<p>Hallo <a href="https://shop.example/x">x</a>
            <IMG alt="Ünïcode ✓" SRC="https://cdn.example/a.png?w=1&amp;h=2">
            <img data-src="https://cdn.example/lazy.png" src='http://cdn.example/b.gif'/>
            <img src=https://cdn.example/c.jpg width=10>
            <img src="cid:logo"><img src="data:image/png;base64,AAAA"><img srcset="https://cdn.example/d.png 2x">"#;
        assert_eq!(
            remote_pictures(html),
            ["https://cdn.example/a.png?w=1&h=2", "http://cdn.example/b.gif", "https://cdn.example/c.jpg"]
        );
        assert!(remote_pictures("<img src=\"https://cdn.example/never-closed").is_empty());
        assert!(remote_pictures("<img").is_empty());
    }
}
