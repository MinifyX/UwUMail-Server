//! Email objects: JSON from stored metadata and parsed MIME, and messages built from JSON.

use std::borrow::Cow;
use std::collections::HashMap;

use mail_builder::MessageBuilder;
use mail_builder::headers::HeaderType;
use mail_builder::headers::address::Address as BuilderAddress;
use mail_builder::headers::content_type::ContentType as BuilderContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::headers::text::Text;
use mail_builder::headers::url::URL;
use mail_builder::mime::{BodyPart, MimePart};
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders, PartType};
use serde_json::{Map, Value, json};
use uwumail_store::{BlobHash, EmailAddress, EmailRecord};

use crate::{dates, ids};

pub const DEFAULT_PROPERTIES: &[&str] = &[
    "id",
    "blobId",
    "threadId",
    "mailboxIds",
    "keywords",
    "size",
    "receivedAt",
    "messageId",
    "inReplyTo",
    "references",
    "sender",
    "from",
    "to",
    "cc",
    "bcc",
    "replyTo",
    "subject",
    "sentAt",
    "hasAttachment",
    "preview",
    "bodyValues",
    "textBody",
    "htmlBody",
    "attachments",
];

pub const DEFAULT_BODY_PROPERTIES: &[&str] =
    &["partId", "blobId", "size", "name", "type", "charset", "disposition", "cid", "language", "location"];

/// Which body parts get their text in `bodyValues`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyValueOptions {
    pub text: bool,
    pub html: bool,
    pub all: bool,
    pub max_bytes: usize,
}

/// Properties that need the raw message.
pub fn needs_raw(properties: &[String]) -> bool {
    properties.iter().any(|p| {
        matches!(
            p.as_str(),
            "headers"
                | "bodyStructure"
                | "bodyValues"
                | "textBody"
                | "htmlBody"
                | "attachments"
                | "uwuSafeHtml"
                | "uwuHasRemoteContent"
        ) || p.starts_with("header:")
    })
}

/// The cleaned HTML body of a message, or nothing when it has none.
fn safe_html_of(message: &Message<'_>) -> Option<String> {
    let index = *message.html_body.first()? as usize;
    let part = message.parts.get(index)?;
    let html = match &part.body {
        PartType::Html(text) => text.as_ref(),
        _ => return None,
    };
    Some(crate::safe_html::sanitize(html))
}

/// EmailAddress objects always have a `name`, `null` when there is none (RFC 8621, 4.1.2.3).
fn addresses_or_null(list: &[EmailAddress]) -> Value {
    if list.is_empty() {
        return Value::Null;
    }
    list.iter().map(|address| json!({ "name": address.name, "email": address.email })).collect()
}

fn ids_or_null(list: &[String]) -> Value {
    if list.is_empty() { Value::Null } else { json!(list) }
}

fn parsed_addresses(address: Option<&Address<'_>>) -> Value {
    let list: Vec<Value> = address
        .map(|a| {
            a.iter()
                .filter_map(|addr| Some(json!({ "name": addr.name.as_deref(), "email": addr.address.as_deref()? })))
                .collect()
        })
        .unwrap_or_default();
    if list.is_empty() { Value::Null } else { Value::Array(list) }
}

/// `asGroupedAddresses`: groups with their names; addresses outside a group are a group with a
/// `null` name (RFC 8621, 4.1.2.4).
fn grouped_addresses(address: Option<&Address<'_>>) -> Value {
    let addresses = |list: &[mail_parser::Addr<'_>]| -> Vec<Value> {
        list.iter()
            .filter_map(|addr| Some(json!({ "name": addr.name.as_deref(), "email": addr.address.as_deref()? })))
            .collect()
    };
    let groups: Vec<Value> = match address {
        None => Vec::new(),
        Some(Address::List(list)) => vec![json!({ "name": null, "addresses": addresses(list) })],
        Some(Address::Group(groups)) => groups
            .iter()
            .map(|group| json!({ "name": group.name.as_deref(), "addresses": addresses(&group.addresses) }))
            .collect(),
    };
    if groups.is_empty() { Value::Null } else { Value::Array(groups) }
}

fn text_list(value: &HeaderValue<'_>) -> Value {
    match value {
        HeaderValue::Text(id) => json!([id.trim_matches(['<', '>'])]),
        HeaderValue::TextList(list) => {
            json!(list.iter().map(|id| id.trim_matches(['<', '>']).to_owned()).collect::<Vec<_>>())
        }
        _ => Value::Null,
    }
}

/// Raw header name and value (after the colon) as they appear in the message.
fn raw_headers<'x>(raw: &'x [u8], part: &mail_parser::MessagePart<'_>) -> Vec<(Cow<'x, str>, Cow<'x, str>)> {
    part.headers
        .iter()
        .filter_map(|header| {
            let field = raw.get(header.offset_field as usize..header.offset_start as usize)?;
            let value = raw.get(header.offset_start as usize..header.offset_end as usize)?;
            let name = String::from_utf8_lossy(field);
            let name = Cow::Owned(name.trim_end_matches(':').trim().to_owned());
            let value = String::from_utf8_lossy(value);
            let value = Cow::Owned(
                value.strip_suffix("\r\n").or_else(|| value.strip_suffix('\n')).unwrap_or(&value).to_owned(),
            );
            Some((name, value))
        })
        .collect()
}

/// `header:Name[:asForm][:all]`
fn header_property(property: &str, headers: &[(Cow<'_, str>, Cow<'_, str>)]) -> Option<Value> {
    let rest = property.strip_prefix("header:")?;
    let mut parts: Vec<&str> = rest.split(':').collect();
    let all = parts.last() == Some(&"all");
    if all {
        parts.pop();
    }
    let form = if parts.len() > 1 && parts.last().is_some_and(|p| p.starts_with("as")) { parts.pop() } else { None };
    let name = parts.join(":");
    let values: Vec<Value> = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(&name))
        .map(|(_, value)| header_form(value, form.unwrap_or("asRaw")))
        .collect();
    Some(if all { Value::Array(values) } else { values.into_iter().last().unwrap_or(Value::Null) })
}

/// Parses a raw header value in one of RFC 8621's forms by letting mail-parser read it
/// under a header name that uses that form.
pub fn header_form(raw_value: &str, form: &str) -> Value {
    let parse = |name: &str| {
        let message = format!("{name}:{raw_value}\r\n\r\n");
        MessageParser::new().parse_headers(message.as_bytes()).map(|m| m.into_owned())
    };
    match form {
        "asText" => parse("Subject").and_then(|m| m.subject().map(|s| json!(s.trim()))).unwrap_or(Value::Null),
        "asAddresses" => parse("To").map(|m| parsed_addresses(m.to())).unwrap_or(Value::Null),
        "asGroupedAddresses" => parse("To").map(|m| grouped_addresses(m.to())).unwrap_or(Value::Null),
        "asMessageIds" => parse("References").map(|m| text_list(m.references())).unwrap_or(Value::Null),
        // A Date keeps the offset the header was written in (RFC 8620, section 1.4).
        "asDate" => parse("Date")
            .and_then(|m| m.date().filter(|d| d.is_valid()).map(|d| json!(d.to_rfc3339())))
            .unwrap_or(Value::Null),
        "asURLs" => {
            let urls: Vec<String> = raw_value
                .split('<')
                .skip(1)
                .filter_map(|chunk| chunk.split_once('>').map(|(url, _)| url.trim().to_owned()))
                .collect();
            if urls.is_empty() { Value::Null } else { json!(urls) }
        }
        _ => json!(raw_value),
    }
}

fn part_type(part: &mail_parser::MessagePart<'_>) -> String {
    match part.content_type() {
        Some(ct) => match &ct.c_subtype {
            Some(sub) => format!("{}/{}", ct.c_type, sub).to_lowercase(),
            None => ct.c_type.to_lowercase(),
        },
        None => match part.body {
            PartType::Text(_) => "text/plain".into(),
            PartType::Html(_) => "text/html".into(),
            PartType::Message(_) => "message/rfc822".into(),
            PartType::Multipart(_) => "multipart/mixed".into(),
            _ => "application/octet-stream".into(),
        },
    }
}

fn part_size(part: &mail_parser::MessagePart<'_>) -> usize {
    match &part.body {
        PartType::Text(text) | PartType::Html(text) => text.len(),
        PartType::Binary(bytes) | PartType::InlineBinary(bytes) => bytes.len(),
        PartType::Message(nested) => nested.raw_message.len(),
        PartType::Multipart(_) => (part.offset_end.saturating_sub(part.offset_body)) as usize,
    }
}

/// Decoded content of a part, for downloads.
pub fn part_content(raw: &[u8], index: usize) -> Option<(Vec<u8>, String)> {
    let message = MessageParser::default().parse(raw)?;
    let part = message.parts.get(index)?;
    let content_type = part_type(part);
    let bytes = match &part.body {
        PartType::Text(text) | PartType::Html(text) => text.as_bytes().to_vec(),
        PartType::Binary(bytes) | PartType::InlineBinary(bytes) => bytes.to_vec(),
        PartType::Message(nested) => nested.raw_message.to_vec(),
        PartType::Multipart(_) => raw.get(part.offset_body as usize..part.offset_end as usize)?.to_vec(),
    };
    Some((bytes, content_type))
}

fn body_part(message: &Message<'_>, hash: &BlobHash, index: usize, properties: &[String], recurse: bool) -> Value {
    let Some(part) = message.parts.get(index) else {
        return Value::Null;
    };
    let mut object = Map::new();
    for property in properties {
        let value = match property.as_str() {
            "partId" => match part.body {
                PartType::Multipart(_) => Value::Null,
                _ => json!(index.to_string()),
            },
            "blobId" => match part.body {
                PartType::Multipart(_) => Value::Null,
                _ => json!(ids::part_blob(hash, index)),
            },
            "size" => json!(part_size(part)),
            "headers" => {
                json!(
                    raw_headers(&message.raw_message, part)
                        .iter()
                        .map(|(n, v)| json!({ "name": n, "value": v }))
                        .collect::<Vec<_>>()
                )
            }
            "name" => json!(part.attachment_name()),
            "type" => json!(part_type(part)),
            // Without a charset parameter a text part is US-ASCII, as MIME says (RFC 8621, 4.1.4).
            "charset" => json!(
                part.content_type().and_then(|ct| ct.attribute("charset")).map(str::to_owned).or_else(|| part_type(
                    part
                )
                .starts_with("text/")
                .then(|| "us-ascii".to_owned()))
            ),
            "subParts" => match part.body {
                // Filled in below.
                PartType::Multipart(_) if recurse => continue,
                _ => Value::Null,
            },
            "disposition" => json!(part.content_disposition().map(|d| d.c_type.to_lowercase())),
            "cid" => json!(part.content_id().map(|c| c.trim_matches(['<', '>']).to_owned())),
            "language" => text_list(part.content_language()),
            "location" => json!(part.content_location()),
            other if other.starts_with("header:") => {
                header_property(other, &raw_headers(&message.raw_message, part)).unwrap_or(Value::Null)
            }
            _ => continue,
        };
        object.insert(property.clone(), value);
    }
    if let PartType::Multipart(children) = &part.body
        && recurse
    {
        let sub: Vec<Value> =
            children.iter().map(|child| body_part(message, hash, *child as usize, properties, true)).collect();
        object.insert("subParts".into(), Value::Array(sub));
    }
    Value::Object(object)
}

fn truncate(text: &str, max: usize) -> (String, bool) {
    if max == 0 || text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_owned(), true)
}

fn body_values(message: &Message<'_>, options: BodyValueOptions) -> Value {
    let mut values = Map::new();
    let mut add = |index: usize| {
        let Some(part) = message.parts.get(index) else { return };
        let (text, problem) = match &part.body {
            PartType::Text(text) | PartType::Html(text) => (text.as_ref(), part.is_encoding_problem),
            _ => return,
        };
        // The value has LF line endings, not the CRLF of the message (RFC 8621, 4.1.4).
        let text = text.replace("\r\n", "\n");
        let (value, truncated) = truncate(&text, options.max_bytes);
        values.insert(
            index.to_string(),
            json!({ "value": value, "isEncodingProblem": problem, "isTruncated": truncated }),
        );
    };
    if options.all {
        for index in 0..message.parts.len() {
            add(index);
        }
    } else {
        if options.text {
            message.text_body.iter().for_each(|i| add(*i as usize));
        }
        if options.html {
            message.html_body.iter().for_each(|i| add(*i as usize));
        }
    }
    Value::Object(values)
}

/// Builds the JSON of an email. `raw` is only needed for body and header properties.
pub fn to_json(
    record: Option<&EmailRecord>,
    raw: Option<&[u8]>,
    hash: &BlobHash,
    properties: &[String],
    body_properties: &[String],
    options: BodyValueOptions,
) -> Value {
    let parsed = raw.and_then(|raw| MessageParser::default().parse(raw));
    let headers =
        parsed.as_ref().and_then(|m| m.parts.first().map(|root| raw_headers(&m.raw_message, root))).unwrap_or_default();
    let mut object = Map::new();
    for property in properties {
        let value = match (property.as_str(), record, parsed.as_ref()) {
            ("id", Some(r), _) => json!(ids::email(r.id)),
            ("id", None, _) => continue,
            ("blobId", _, _) => json!(ids::blob(hash)),
            ("threadId", Some(r), _) => json!(ids::thread(r.thread_id)),
            ("mailboxIds", Some(r), _) => {
                Value::Object(r.mailbox_ids.iter().map(|m| (ids::mailbox(*m), Value::Bool(true))).collect())
            }
            ("keywords", Some(r), _) => {
                Value::Object(r.keywords.iter().map(|k| (k.clone(), Value::Bool(true))).collect())
            }
            ("size", Some(r), _) => json!(r.size),
            ("size", None, _) => json!(raw.map_or(0, <[u8]>::len)),
            ("receivedAt", Some(r), _) => json!(dates::format(r.received_at)),
            ("threadId" | "mailboxIds" | "keywords" | "receivedAt", None, _) => Value::Null,
            ("messageId", Some(r), _) => r.message_id.as_ref().map_or(Value::Null, |id| json!([id])),
            ("inReplyTo", Some(r), _) => ids_or_null(&r.in_reply_to),
            ("references", Some(r), _) => ids_or_null(&r.references),
            ("sender", Some(r), _) => addresses_or_null(&r.sender),
            ("from", Some(r), _) => addresses_or_null(&r.from),
            ("to", Some(r), _) => addresses_or_null(&r.to),
            ("cc", Some(r), _) => addresses_or_null(&r.cc),
            ("bcc", Some(r), _) => addresses_or_null(&r.bcc),
            ("replyTo", Some(r), _) => addresses_or_null(&r.reply_to),
            ("subject", Some(r), _) => {
                if r.subject.is_empty() {
                    Value::Null
                } else {
                    json!(r.subject)
                }
            }
            ("sentAt", Some(r), _) => r.sent_at.map_or(Value::Null, |t| json!(dates::format(t))),
            ("hasAttachment", Some(r), _) => json!(r.has_attachment),
            ("preview", Some(r), _) => json!(r.preview),
            ("messageId", None, Some(m)) => m.message_id().map_or(Value::Null, |id| json!([id])),
            ("inReplyTo", None, Some(m)) => text_list(m.in_reply_to()),
            ("references", None, Some(m)) => text_list(m.references()),
            ("sender", None, Some(m)) => parsed_addresses(m.sender()),
            ("from", None, Some(m)) => parsed_addresses(m.from()),
            ("to", None, Some(m)) => parsed_addresses(m.to()),
            ("cc", None, Some(m)) => parsed_addresses(m.cc()),
            ("bcc", None, Some(m)) => parsed_addresses(m.bcc()),
            ("replyTo", None, Some(m)) => parsed_addresses(m.reply_to()),
            ("subject", None, Some(m)) => json!(m.subject()),
            ("sentAt", None, Some(m)) => m.date().map_or(Value::Null, |d| json!(dates::format(d.to_timestamp()))),
            ("hasAttachment", None, Some(m)) => json!(m.attachment_count() > 0),
            ("preview", None, Some(m)) => {
                json!(m.body_preview(256).map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ")))
            }
            ("headers", _, Some(_)) => {
                json!(headers.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect::<Vec<_>>())
            }
            ("bodyStructure", _, Some(m)) => body_part(m, hash, 0, body_properties, true),
            ("textBody", _, Some(m)) => json!(
                m.text_body.iter().map(|i| body_part(m, hash, *i as usize, body_properties, false)).collect::<Vec<_>>()
            ),
            ("htmlBody", _, Some(m)) => json!(
                m.html_body.iter().map(|i| body_part(m, hash, *i as usize, body_properties, false)).collect::<Vec<_>>()
            ),
            ("attachments", _, Some(m)) => {
                json!(
                    m.attachments
                        .iter()
                        .map(|i| body_part(m, hash, *i as usize, body_properties, false))
                        .collect::<Vec<_>>()
                )
            }
            ("bodyValues", _, Some(m)) => body_values(m, options),
            ("uwuSafeHtml", _, Some(m)) => safe_html_of(m).map_or(Value::Null, Value::String),
            ("uwuHasRemoteContent", _, Some(m)) => {
                json!(safe_html_of(m).is_some_and(|clean| crate::safe_html::has_remote_content(&clean)))
            }
            (other, _, Some(_)) if other.starts_with("header:") => {
                header_property(other, &headers).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        };
        object.insert(property.clone(), value);
    }
    Value::Object(object)
}

/// A problem with an Email object sent by a client.
#[derive(Debug)]
pub struct BuildError {
    pub properties: Vec<String>,
    pub description: String,
}

fn build_error(property: &str, description: impl Into<String>) -> BuildError {
    BuildError { properties: vec![property.to_owned()], description: description.into() }
}

fn build_errors(properties: Vec<String>, description: impl Into<String>) -> BuildError {
    BuildError { properties, description: description.into() }
}

fn address_list(value: &Value, property: &str) -> Result<Vec<BuilderAddress<'static>>, BuildError> {
    let list: Vec<EmailAddress> =
        serde_json::from_value(value.clone()).map_err(|_| build_error(property, "must be a list of {name, email}"))?;
    Ok(list.into_iter().map(|a| BuilderAddress::new_address(a.name.map(Cow::Owned), Cow::Owned(a.email))).collect())
}

fn builder_addresses(value: &Value, property: &str) -> Result<Option<BuilderAddress<'static>>, BuildError> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(BuilderAddress::new_list(address_list(value, property)?)))
}

fn builder_ids(value: &Value, property: &str) -> Result<Option<MessageId<'static>>, BuildError> {
    if value.is_null() {
        return Ok(None);
    }
    let list: Vec<String> =
        serde_json::from_value(value.clone()).map_err(|_| build_error(property, "must be a list of ids"))?;
    Ok(Some(MessageId::new_list(list.into_iter().map(Cow::Owned))))
}

/// The spelling mail-builder and everyone else expect for the common header fields.
fn canonical_header_name(name: &str) -> String {
    const KNOWN: &[&str] = &[
        "From",
        "Sender",
        "Reply-To",
        "To",
        "Cc",
        "Bcc",
        "Subject",
        "Date",
        "Message-ID",
        "In-Reply-To",
        "References",
        "Comments",
        "Keywords",
        "Resent-Date",
        "Resent-From",
        "Resent-Sender",
        "Resent-To",
        "Resent-Cc",
        "Resent-Bcc",
        "Resent-Message-ID",
        "List-Id",
        "List-Help",
        "List-Unsubscribe",
        "List-Subscribe",
        "List-Post",
        "List-Owner",
        "List-Archive",
    ];
    KNOWN.iter().find(|known| known.eq_ignore_ascii_case(name)).map_or_else(|| name.to_owned(), |k| (*k).to_owned())
}

/// The parsed forms a header field may be given in (RFC 8621, section 4.1.2); fields not listed
/// there take any form.
fn form_allowed(name: &str, form: &str) -> bool {
    let is = |list: &[&str]| list.iter().any(|n| n.eq_ignore_ascii_case(name));
    const ADDRESSES: &[&str] = &[
        "From",
        "Sender",
        "Reply-To",
        "To",
        "Cc",
        "Bcc",
        "Resent-From",
        "Resent-Sender",
        "Resent-Reply-To",
        "Resent-To",
        "Resent-Cc",
        "Resent-Bcc",
    ];
    const TEXT: &[&str] = &["Subject", "Comments", "Keywords", "List-Id"];
    const IDS: &[&str] = &["Message-ID", "In-Reply-To", "References", "Resent-Message-ID"];
    const DATES: &[&str] = &["Date", "Resent-Date"];
    const URLS: &[&str] =
        &["List-Help", "List-Unsubscribe", "List-Subscribe", "List-Post", "List-Owner", "List-Archive"];
    let known = [ADDRESSES, TEXT, IDS, DATES, URLS].iter().any(|list| is(list));
    match form {
        "asRaw" => true,
        _ if !known => true,
        "asText" => is(TEXT),
        "asAddresses" | "asGroupedAddresses" => is(ADDRESSES),
        "asMessageIds" => is(IDS),
        "asDate" => is(DATES),
        "asURLs" => is(URLS),
        _ => false,
    }
}

/// A `header:Name[:asForm][:all]` property split into its name, form and whether it is `:all`.
fn header_key(key: &str) -> Option<(String, &str, bool)> {
    let rest = key.strip_prefix("header:")?;
    let mut parts: Vec<&str> = rest.split(':').collect();
    let all = parts.len() > 1 && parts.last() == Some(&"all");
    if all {
        parts.pop();
    }
    let form = if parts.len() > 1 && parts.last().is_some_and(|p| p.starts_with("as")) {
        parts.pop().unwrap_or("asRaw")
    } else {
        "asRaw"
    };
    if parts.len() != 1 || parts[0].is_empty() {
        return None;
    }
    Some((parts[0].to_owned(), form, all))
}

/// One header value in one of the parsed forms, for mail-builder.
fn header_value(key: &str, form: &str, value: &Value) -> Result<HeaderType<'static>, BuildError> {
    let wrong = || build_error(key, format!("not a valid value for {form}"));
    Ok(match form {
        // The raw value starts after the colon, usually with a space mail-builder writes itself.
        "asRaw" => {
            let raw = value.as_str().ok_or_else(wrong)?;
            Raw::new(raw.strip_prefix([' ', '\t']).unwrap_or(raw).to_owned()).into()
        }
        "asText" => Text::new(value.as_str().ok_or_else(wrong)?.to_owned()).into(),
        "asAddresses" => BuilderAddress::new_list(address_list(value, key)?).into(),
        "asGroupedAddresses" => {
            let groups = value.as_array().ok_or_else(wrong)?;
            let mut list = Vec::new();
            for group in groups {
                let addresses = address_list(group.get("addresses").unwrap_or(&Value::Null), key)?;
                match group.get("name").and_then(Value::as_str) {
                    Some(name) => list.push(BuilderAddress::new_group(Some(name.to_owned()), addresses)),
                    None => list.extend(addresses),
                }
            }
            BuilderAddress::new_list(list).into()
        }
        "asMessageIds" => {
            let ids: Vec<String> = serde_json::from_value(value.clone()).map_err(|_| wrong())?;
            MessageId::new_list(ids.into_iter().map(Cow::Owned)).into()
        }
        "asDate" => Date::new(value.as_str().and_then(dates::parse).ok_or_else(wrong)?).into(),
        "asURLs" => {
            let urls: Vec<String> = serde_json::from_value(value.clone()).map_err(|_| wrong())?;
            URL::new_list(urls.into_iter().map(Cow::Owned)).into()
        }
        _ => return Err(build_error(key, format!("unknown header form {form}"))),
    })
}

/// The header fields given as `header:` properties of an Email or a body part, each with its
/// values (several with `:all`).
fn header_properties(object: &Map<String, Value>) -> Result<Vec<(String, Vec<HeaderType<'static>>)>, BuildError> {
    let mut headers = Vec::new();
    for (key, value) in object {
        if !key.starts_with("header:") {
            continue;
        }
        let (name, form, all) = header_key(key).ok_or_else(|| build_error(key, "not a header property"))?;
        if !form_allowed(&name, form) {
            return Err(build_error(key, format!("{name} cannot be given {form}")));
        }
        let values = if all {
            let list = value.as_array().ok_or_else(|| build_error(key, ":all takes a list"))?;
            list.iter().map(|v| header_value(key, form, v)).collect::<Result<Vec<_>, _>>()?
        } else if value.is_null() {
            Vec::new()
        } else {
            vec![header_value(key, form, value)?]
        };
        headers.push((canonical_header_name(&name), values));
    }
    Ok(headers)
}

/// Checks a create object against the rules of RFC 8621, section 4.6, before anything is built.
pub fn validate_create(object: &Map<String, Value>) -> Result<(), BuildError> {
    if object.contains_key("headers") {
        return Err(build_error("headers", "give each header field as a header: property instead"));
    }
    // Every header field in one representation only.
    let mut seen: HashMap<String, String> = HashMap::new();
    let convenience = [
        ("from", "From"),
        ("sender", "Sender"),
        ("replyTo", "Reply-To"),
        ("to", "To"),
        ("cc", "Cc"),
        ("bcc", "Bcc"),
        ("subject", "Subject"),
        ("sentAt", "Date"),
        ("messageId", "Message-ID"),
        ("inReplyTo", "In-Reply-To"),
        ("references", "References"),
    ];
    for (property, field) in convenience {
        if object.contains_key(property) {
            seen.insert(field.to_ascii_lowercase(), property.to_owned());
        }
    }
    for key in object.keys() {
        let Some((name, _, _)) = header_key(key) else { continue };
        if name.to_ascii_lowercase().starts_with("content-") {
            return Err(build_error(key, "Content-* header fields belong to body parts"));
        }
        if seen.insert(name.to_ascii_lowercase(), key.clone()).is_some() {
            return Err(build_error(key, format!("{name} is given twice")));
        }
    }
    let structural: Vec<String> = ["textBody", "htmlBody", "attachments"]
        .iter()
        .filter(|key| object.get(**key).is_some_and(|v| !v.is_null()))
        .map(|key| (*key).to_owned())
        .collect();
    if object.get("bodyStructure").is_some_and(|v| !v.is_null()) && !structural.is_empty() {
        return Err(build_errors(structural, "give either bodyStructure or textBody, htmlBody and attachments"));
    }
    for (key, wanted) in [("textBody", "text/plain"), ("htmlBody", "text/html")] {
        let Some(list) = object.get(key).filter(|v| !v.is_null()) else { continue };
        let list = list.as_array().ok_or_else(|| build_error(key, "must be a list of body parts"))?;
        if list.len() > 1 {
            return Err(build_error(key, "may have one part only"));
        }
        if let Some(kind) = list.first().and_then(|part| part.get("type")).and_then(Value::as_str)
            && !kind.eq_ignore_ascii_case(wanted)
        {
            return Err(build_error(key, format!("the part must be {wanted}")));
        }
    }
    if let Some(values) = object.get("bodyValues").and_then(Value::as_object) {
        let mut flagged = Vec::new();
        for (id, value) in values {
            for flag in ["isEncodingProblem", "isTruncated"] {
                if value.get(flag).and_then(Value::as_bool) == Some(true) {
                    flagged.push(format!("bodyValues/{id}/{flag}"));
                }
            }
        }
        if !flagged.is_empty() {
            return Err(build_errors(flagged, "isEncodingProblem and isTruncated must be false"));
        }
    }
    let mut parts = Vec::new();
    if let Some(structure) = object.get("bodyStructure").filter(|v| !v.is_null()) {
        collect_parts(structure, "bodyStructure", &mut parts);
    }
    for key in ["textBody", "htmlBody", "attachments"] {
        if let Some(list) = object.get(key).and_then(Value::as_array) {
            for (index, part) in list.iter().enumerate() {
                collect_parts(part, &format!("{key}/{index}"), &mut parts);
            }
        }
    }
    for (path, part) in parts {
        validate_part(&path, part)?;
    }
    Ok(())
}

fn collect_parts<'a>(part: &'a Value, path: &str, out: &mut Vec<(String, &'a Value)>) {
    out.push((path.to_owned(), part));
    if let Some(children) = part.get("subParts").and_then(Value::as_array) {
        for (index, child) in children.iter().enumerate() {
            collect_parts(child, &format!("{path}/subParts/{index}"), out);
        }
    }
}

fn validate_part(path: &str, part: &Value) -> Result<(), BuildError> {
    let object = part.as_object().ok_or_else(|| build_error(path, "a body part is an object"))?;
    let at = |property: &str| format!("{path}/{property}");
    if object.contains_key("headers") {
        return Err(build_error(&at("headers"), "give each header field as a header: property instead"));
    }
    let mut seen: HashMap<String, String> = HashMap::new();
    for (property, field) in [
        ("type", "content-type"),
        ("charset", "content-type"),
        ("disposition", "content-disposition"),
        ("name", "content-disposition"),
        ("cid", "content-id"),
        ("language", "content-language"),
        ("location", "content-location"),
    ] {
        if object.get(property).is_some_and(|v| !v.is_null()) {
            seen.insert(field.to_owned(), property.to_owned());
        }
    }
    for key in object.keys() {
        let Some((name, _, _)) = header_key(key) else { continue };
        let lower = name.to_ascii_lowercase();
        if lower == "content-transfer-encoding" {
            return Err(build_error(&at(key), "the server chooses the Content-Transfer-Encoding"));
        }
        if lower.starts_with("content-") {
            return Err(build_error(
                &at(key),
                "give Content-* fields as type, charset, disposition, name, cid, language and location",
            ));
        }
        if seen.insert(lower, key.clone()).is_some() {
            return Err(build_error(&at(key), format!("{name} is given twice")));
        }
    }
    let is_multipart = object.get("subParts").is_some_and(|v| !v.is_null());
    let has = |key: &str| object.get(key).is_some_and(|v| !v.is_null());
    if !is_multipart {
        if has("partId") && has("blobId") {
            return Err(build_errors(vec![at("partId"), at("blobId")], "give a partId or a blobId, not both"));
        }
        if has("partId") {
            for property in ["charset", "size"] {
                if has(property) {
                    return Err(build_error(&at(property), format!("{property} is set by the server for a partId")));
                }
            }
        }
    }
    Ok(())
}

/// Resolves the bytes behind a body part given by `partId` (from `bodyValues`) or `blobId`.
pub trait BlobSource {
    fn blob(&self, blob_id: &str) -> Option<Vec<u8>>;
}

fn leaf_part(
    part: &Value,
    body_values: &Map<String, Value>,
    blobs: &dyn BlobSource,
) -> Result<MimePart<'static>, BuildError> {
    let content_type = part.get("type").and_then(Value::as_str);
    let body: BodyPart<'static> = if let Some(part_id) = part.get("partId").and_then(Value::as_str) {
        let value = body_values
            .get(part_id)
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .ok_or_else(|| build_error("bodyValues", format!("no body value for part {part_id}")))?;
        BodyPart::Text(Cow::Owned(value.to_owned()))
    } else if let Some(blob_id) = part.get("blobId").and_then(Value::as_str) {
        let bytes = blobs.blob(blob_id).ok_or_else(|| build_error("blobId", format!("blob {blob_id} not found")))?;
        if let Some(size) = part.get("size").and_then(Value::as_u64)
            && size != bytes.len() as u64
        {
            return Err(build_error("size", format!("the blob has {} bytes", bytes.len())));
        }
        BodyPart::Binary(Cow::Owned(bytes))
    } else {
        return Err(build_error("bodyStructure", "every part needs a partId or a blobId"));
    };
    let content_type = content_type.map(str::to_owned).unwrap_or_else(|| match body {
        BodyPart::Text(_) => "text/plain".into(),
        _ => "application/octet-stream".into(),
    });
    let content_type = content_type.to_lowercase();
    let mut builder_type = BuilderContentType::new(content_type.clone());
    if let Some(charset) = part.get("charset").and_then(Value::as_str) {
        builder_type = builder_type.attribute("charset", charset.to_owned());
    } else if matches!(body, BodyPart::Text(_)) && content_type.starts_with("text/") {
        // Text from bodyValues is written as UTF-8.
        builder_type = builder_type.attribute("charset", "utf-8");
    }
    let mut mime = MimePart::new(builder_type, body);
    match (part.get("disposition").and_then(Value::as_str), part.get("name").and_then(Value::as_str)) {
        (Some("inline"), _) => mime = mime.inline(),
        (Some("attachment"), name) | (None, name @ Some(_)) => {
            mime = mime.attachment(name.unwrap_or("attachment").to_owned())
        }
        _ => {}
    }
    if let Some(cid) = part.get("cid").and_then(Value::as_str) {
        mime = mime.cid(cid.to_owned());
    }
    if let Some(language) = part.get("language").and_then(Value::as_array) {
        let tags: Vec<&str> = language.iter().filter_map(Value::as_str).collect();
        if !tags.is_empty() {
            mime = mime.header("Content-Language", Raw::new(tags.join(", ")));
        }
    }
    if let Some(location) = part.get("location").and_then(Value::as_str) {
        mime = mime.header("Content-Location", Raw::new(location.to_owned()));
    }
    for (name, values) in header_properties(part.as_object().expect("validated as an object"))? {
        for value in values {
            mime = mime.header(name.clone(), value);
        }
    }
    Ok(mime)
}

fn structure_part(
    part: &Value,
    body_values: &Map<String, Value>,
    blobs: &dyn BlobSource,
) -> Result<MimePart<'static>, BuildError> {
    match part.get("subParts").and_then(Value::as_array) {
        Some(children) => {
            let content_type = part.get("type").and_then(Value::as_str).unwrap_or("multipart/mixed").to_lowercase();
            let children = children
                .iter()
                .map(|child| structure_part(child, body_values, blobs))
                .collect::<Result<Vec<_>, _>>()?;
            let mut mime = MimePart::new(BuilderContentType::new(content_type), children);
            for (name, values) in header_properties(part.as_object().expect("validated as an object"))? {
                for value in values {
                    mime = mime.header(name.clone(), value);
                }
            }
            Ok(mime)
        }
        None => leaf_part(part, body_values, blobs),
    }
}

/// Builds an RFC 5322 message from a JMAP Email create object.
pub fn build_message(object: &Map<String, Value>, blobs: &dyn BlobSource) -> Result<Vec<u8>, BuildError> {
    validate_create(object)?;
    let empty = Map::new();
    let body_values = object.get("bodyValues").and_then(Value::as_object).unwrap_or(&empty);
    let mut builder = MessageBuilder::new();
    let null = Value::Null;
    let get = |key: &str| object.get(key).unwrap_or(&null);

    if let Some(from) = builder_addresses(get("from"), "from")? {
        builder = builder.from(from);
    }
    if let Some(to) = builder_addresses(get("to"), "to")? {
        builder = builder.to(to);
    }
    if let Some(cc) = builder_addresses(get("cc"), "cc")? {
        builder = builder.cc(cc);
    }
    if let Some(bcc) = builder_addresses(get("bcc"), "bcc")? {
        builder = builder.bcc(bcc);
    }
    if let Some(reply_to) = builder_addresses(get("replyTo"), "replyTo")? {
        builder = builder.reply_to(reply_to);
    }
    if let Some(sender) = builder_addresses(get("sender"), "sender")? {
        builder = builder.sender(sender);
    }
    if let Some(subject) = get("subject").as_str() {
        builder = builder.subject(subject.to_owned());
    }
    let headers = header_properties(object)?;
    // A Date given as header:Date is the one; otherwise sentAt, or now.
    if !headers.iter().any(|(name, _)| name == "Date") {
        let sent_at = match get("sentAt") {
            Value::String(date) => dates::parse(date).ok_or_else(|| build_error("sentAt", "must be a date"))?,
            _ => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
        };
        builder = builder.date(Date::new(sent_at));
    }
    if let Some(ids) = builder_ids(get("messageId"), "messageId")? {
        builder = builder.message_id(ids);
    }
    if let Some(ids) = builder_ids(get("inReplyTo"), "inReplyTo")? {
        builder = builder.in_reply_to(ids);
    }
    if let Some(ids) = builder_ids(get("references"), "references")? {
        builder = builder.references(ids);
    }
    for (name, values) in headers {
        builder = builder.headers(name, values);
    }

    if let Some(structure) = object.get("bodyStructure").filter(|v| !v.is_null()) {
        builder = builder.body(structure_part(structure, body_values, blobs)?);
    } else {
        let list = |key: &str| object.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        let text = list("textBody");
        let html = list("htmlBody");
        let attachments = list("attachments");
        let mut alternatives = Vec::new();
        if let Some(part) = text.first() {
            let mut part = part.clone();
            part.as_object_mut().map(|p| p.entry("type").or_insert(json!("text/plain")));
            alternatives.push(leaf_part(&part, body_values, blobs)?);
        }
        if let Some(part) = html.first() {
            let mut part = part.clone();
            part.as_object_mut().map(|p| p.entry("type").or_insert(json!("text/html")));
            alternatives.push(leaf_part(&part, body_values, blobs)?);
        }
        let main = match alternatives.len() {
            0 => MimePart::new(
                BuilderContentType::new("text/plain").attribute("charset", "utf-8"),
                BodyPart::Text(Cow::Borrowed("")),
            ),
            1 => alternatives.pop().expect("one part"),
            _ => MimePart::new(BuilderContentType::new("multipart/alternative"), alternatives),
        };
        if attachments.is_empty() {
            builder = builder.body(main);
        } else {
            let mut parts = vec![main];
            for attachment in &attachments {
                parts.push(leaf_part(attachment, body_values, blobs)?);
            }
            builder = builder.body(MimePart::new(BuilderContentType::new("multipart/mixed"), parts));
        }
    }
    builder.write_to_vec().map_err(|err| build_error("bodyStructure", err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoBlobs;
    impl BlobSource for NoBlobs {
        fn blob(&self, _: &str) -> Option<Vec<u8>> {
            Some(b"%PDF".to_vec())
        }
    }

    const MESSAGE: &[u8] =
        b"From: Nyu <nyu@example.org>\r\nTo: mini@example.org\r\nSubject: =?utf-8?q?Gr=C3=BC=C3=9Fe?=\r\n\
X-Mood: happy\r\nList-Unsubscribe: <https://example.org/u>, <mailto:u@example.org>\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHallo Mini\r\n\
--b\r\nContent-Type: application/pdf; name=rechnung.pdf\r\nContent-Disposition: attachment; filename=rechnung.pdf\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERg==\r\n--b--\r\n";

    #[test]
    fn body_and_header_properties() {
        let hash = BlobHash::of(MESSAGE);
        let properties: Vec<String> = [
            "blobId",
            "subject",
            "from",
            "header:X-Mood:asText",
            "header:List-Unsubscribe:asURLs",
            "header:subject",
            "textBody",
            "attachments",
            "bodyValues",
            "bodyStructure",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let body_properties: Vec<String> = DEFAULT_BODY_PROPERTIES.iter().map(|s| s.to_string()).collect();
        let options = BodyValueOptions { text: true, ..Default::default() };
        let json = to_json(None, Some(MESSAGE), &hash, &properties, &body_properties, options);
        assert_eq!(json["subject"], "Grüße");
        assert_eq!(json["from"][0]["email"], "nyu@example.org");
        assert_eq!(json["header:X-Mood:asText"], "happy");
        assert_eq!(json["header:List-Unsubscribe:asURLs"], json!(["https://example.org/u", "mailto:u@example.org"]));
        assert_eq!(json["header:subject"], " =?utf-8?q?Gr=C3=BC=C3=9Fe?=");
        let attachment = &json["attachments"][0];
        assert_eq!(attachment["name"], "rechnung.pdf");
        assert_eq!(attachment["type"], "application/pdf");
        let part_id: usize = attachment["partId"].as_str().unwrap().parse().unwrap();
        assert_eq!(part_content(MESSAGE, part_id).unwrap().0, b"%PDF");
        let text_id = json["textBody"][0]["partId"].as_str().unwrap();
        assert!(json["bodyValues"][text_id]["value"].as_str().unwrap().starts_with("Hallo Mini"));
        assert_eq!(json["bodyStructure"]["type"], "multipart/mixed");
        assert_eq!(json["bodyStructure"]["subParts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn builds_messages_from_json() {
        let object = json!({
            "from": [{ "name": "Mini", "email": "mini@example.org" }],
            "to": [{ "email": "nyu@example.org" }],
            "subject": "Entwurf ✉",
            "header:X-Mood": " sleepy",
            "bodyValues": { "t": { "value": "Hallo Nyu" }, "h": { "value": "<p>Hallo Nyu</p>" } },
            "textBody": [{ "partId": "t" }],
            "htmlBody": [{ "partId": "h" }],
            "attachments": [{ "blobId": "b1", "type": "application/pdf", "name": "a.pdf" }]
        });
        let raw = build_message(object.as_object().unwrap(), &NoBlobs).unwrap();
        let parsed = MessageParser::default().parse(&raw).unwrap();
        assert_eq!(parsed.subject(), Some("Entwurf ✉"));
        assert_eq!(parsed.body_text(0).as_deref(), Some("Hallo Nyu"));
        assert_eq!(parsed.attachment_count(), 1);
        assert_eq!(parsed.header_raw("X-Mood").map(str::trim), Some("sleepy"));
    }
}
