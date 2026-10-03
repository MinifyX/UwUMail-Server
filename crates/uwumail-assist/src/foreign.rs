//! Mail of the person's other accounts (Exchange, Gmail, IMAP), which the UwUMail app sends along
//! because the server does not have it (docs/jmap-assist.md, "Foreign mail"). It is checked
//! strictly, goes to the model exactly like the person's own mail, and nothing of it is kept.

use serde_json::{Map, Value};
use uwumail_store::EmailAddress;

use crate::mail::{MailText, cap, without_quotes};
use crate::{AssistError, Result};

/// Mails of one conversation `Assist/summarize` takes, at most.
pub const MAX_FOREIGN_MAILS: usize = 20;
/// Labels `AssistLabel/suggest` takes along with a foreign mail, at most.
pub const MAX_FOREIGN_LABELS: usize = 50;
const MAX_ADDRESSES: usize = 50;
const MAX_ADDRESS_NAME_CHARS: usize = 200;
const MAX_ADDRESS_CHARS: usize = 320;
const MAX_SUBJECT_CHARS: usize = 998;
const MAX_TEXT_CHARS: usize = 200_000;
const MAX_HEADERS: usize = 100;
const MAX_HEADER_NAME_CHARS: usize = 100;
const MAX_HEADER_VALUE_CHARS: usize = 2000;
/// The spam check's trace headers may be longer: an app sends them whole up to this length and
/// leaves a longer one out, never cut, since a cut one can not be told from a whole one (security
/// review 0.22 R4 I-4, client review C4-1).
const MAX_TRACE_HEADER_VALUE_CHARS: usize = 16_000;
const TRACE_HEADERS: [&str; 3] = ["Authentication-Results", "Received", "X-Spam-Status"];
const MAX_LABEL_NAME_CHARS: usize = 40;
const MAX_LABEL_DESCRIPTION_CHARS: usize = 300;

/// One mail of another account.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForeignMail {
    pub from: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    /// Unix seconds, when it was sent.
    pub date: Option<i64>,
    pub subject: String,
    pub text: String,
    /// For the spam check only.
    pub headers: Vec<(String, String)>,
    /// For the spam check only.
    pub in_junk: bool,
}

impl ForeignMail {
    /// The mail for a prompt: the quoted history removed and cut to `max_chars`, as for own mail.
    pub fn mail_text(&self, max_chars: usize) -> MailText {
        MailText {
            subject: self.subject.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            cc: self.cc.clone(),
            date: self.date.unwrap_or_else(crate::now),
            text: cap(&without_quotes(&self.text), max_chars),
            links: Vec::new(),
            headers: self.headers.clone(),
        }
    }
}

/// One label of another account, as the app keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForeignLabel {
    pub name: String,
    pub description: String,
    pub is_set: bool,
}

fn bad(description: String) -> AssistError {
    AssistError::Invalid { code: "invalidArguments", property: "foreignMails", description }
}

fn bad_label(description: String) -> AssistError {
    AssistError::Invalid { code: "invalidArguments", property: "foreignLabels", description }
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn object<'a>(value: &'a Value, at: &str, error: fn(String) -> AssistError) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| error(format!("{at} is not an object")))
}

/// A string field of at most `max` characters; `None` when absent or `null`.
fn string(object: &Map<String, Value>, at: &str, field: &str, max: usize) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if chars(text) <= max => Ok(Some(text.clone())),
        Some(Value::String(_)) => Err(bad(format!("{at}.{field} is longer than {max} characters"))),
        Some(_) => Err(bad(format!("{at}.{field} is not a string"))),
    }
}

fn addresses(object: &Map<String, Value>, at: &str, field: &str) -> Result<Vec<EmailAddress>> {
    let list = match object.get(field) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(bad(format!("{at}.{field} is not a list"))),
    };
    if list.len() > MAX_ADDRESSES {
        return Err(bad(format!("{at}.{field} has more than {MAX_ADDRESSES} addresses")));
    }
    let mut out = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        let here = format!("{at}.{field}[{index}]");
        let entry = object_of(entry, &here)?;
        let name = string(entry, &here, "name", MAX_ADDRESS_NAME_CHARS)?;
        let email = string(entry, &here, "email", MAX_ADDRESS_CHARS)?.unwrap_or_default();
        out.push(EmailAddress { name, email });
    }
    Ok(out)
}

fn object_of<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>> {
    object(value, at, bad)
}

/// Reads `foreignMails`: `count` says how many the call takes.
pub fn foreign_mails(value: &Value, count: std::ops::RangeInclusive<usize>) -> Result<Vec<ForeignMail>> {
    let list = value.as_array().ok_or_else(|| bad("foreignMails is not a list".into()))?;
    if !count.contains(&list.len()) {
        let wanted = if count.start() == count.end() {
            format!("exactly {}", count.start())
        } else {
            format!("{} to {}", count.start(), count.end())
        };
        return Err(bad(format!("this call takes {wanted} foreign mails")));
    }
    let mut out = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        let at = format!("foreignMails[{index}]");
        let mail = object_of(entry, &at)?;
        let date = match mail.get("date") {
            None | Some(Value::Null) => None,
            Some(Value::String(date)) => Some(
                chrono::DateTime::parse_from_rfc3339(date)
                    .map_err(|_| bad(format!("{at}.date is not a UTCDate")))?
                    .timestamp(),
            ),
            Some(_) => return Err(bad(format!("{at}.date is not a UTCDate"))),
        };
        let headers = match mail.get("headers") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(list)) if list.len() <= MAX_HEADERS => {
                let mut headers = Vec::new();
                for (n, header) in list.iter().enumerate() {
                    let here = format!("{at}.headers[{n}]");
                    let header = object_of(header, &here)?;
                    let name = string(header, &here, "name", MAX_HEADER_NAME_CHARS)?.unwrap_or_default();
                    let max = if TRACE_HEADERS.iter().any(|trace| trace.eq_ignore_ascii_case(name.trim())) {
                        MAX_TRACE_HEADER_VALUE_CHARS
                    } else {
                        MAX_HEADER_VALUE_CHARS
                    };
                    let value = string(header, &here, "value", max)?.unwrap_or_default();
                    let value = value
                        .split(['\r', '\n'])
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    headers.push((name.trim().to_owned(), value.trim().to_owned()));
                }
                headers
            }
            Some(Value::Array(_)) => return Err(bad(format!("{at}.headers has more than {MAX_HEADERS} headers"))),
            Some(_) => return Err(bad(format!("{at}.headers is not a list"))),
        };
        let in_junk = match mail.get("inJunk") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(on)) => *on,
            Some(_) => return Err(bad(format!("{at}.inJunk is not a boolean"))),
        };
        out.push(ForeignMail {
            from: addresses(mail, &at, "from")?,
            to: addresses(mail, &at, "to")?,
            cc: addresses(mail, &at, "cc")?,
            date,
            subject: string(mail, &at, "subject", MAX_SUBJECT_CHARS)?.unwrap_or_default(),
            text: string(mail, &at, "text", MAX_TEXT_CHARS)?.unwrap_or_default(),
            headers,
            in_junk,
        });
    }
    Ok(out)
}

/// Reads `foreignLabels`.
pub fn foreign_labels(value: &Value) -> Result<Vec<ForeignLabel>> {
    let list = match value {
        Value::Null => return Ok(Vec::new()),
        Value::Array(list) => list,
        _ => return Err(bad_label("foreignLabels is not a list".into())),
    };
    if list.len() > MAX_FOREIGN_LABELS {
        return Err(bad_label(format!("foreignLabels has more than {MAX_FOREIGN_LABELS} labels")));
    }
    let mut out: Vec<ForeignLabel> = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        let at = format!("foreignLabels[{index}]");
        let label = object(entry, &at, bad_label)?;
        let name = label.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if name.is_empty() || chars(name) > MAX_LABEL_NAME_CHARS || name.chars().any(char::is_control) {
            return Err(bad_label(format!("{at}.name must be 1 to {MAX_LABEL_NAME_CHARS} characters")));
        }
        if out.iter().any(|other| other.name.to_lowercase() == name.to_lowercase()) {
            return Err(bad_label(format!("{at}.name is there twice")));
        }
        let description = match label.get("description") {
            None | Some(Value::Null) => "",
            Some(Value::String(text)) if chars(text) <= MAX_LABEL_DESCRIPTION_CHARS => text.as_str(),
            Some(_) => {
                return Err(bad_label(format!(
                    "{at}.description must be a string of at most {MAX_LABEL_DESCRIPTION_CHARS} characters"
                )));
            }
        };
        let is_set = match label.get("isSet") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(on)) => *on,
            Some(_) => return Err(bad_label(format!("{at}.isSet is not a boolean"))),
        };
        out.push(ForeignLabel { name: name.to_owned(), description: description.trim().to_owned(), is_set });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn foreign_mail_is_checked_strictly() {
        let mail = json!([{
            "from": [{ "name": "Stadtwerke", "email": "rechnung@stadtwerke.example" }],
            "to": [{ "name": null, "email": "leni@example.org" }],
            "date": "2026-09-30T08:12:00Z",
            "subject": "Ihre Rechnung",
            "text": "Guten Tag\n\nAm Mo., 5. Okt. 2026 um 10:00 Uhr schrieb Leni <leni@example.org>:\n> alt",
            "headers": [{ "name": "Authentication-Results", "value": "mx.example.net;\r\n spf=pass" }],
            "inJunk": true
        }]);
        let read = foreign_mails(&mail, 1..=1).unwrap();
        assert_eq!(read[0].date, Some(1_790_755_920));
        assert_eq!(read[0].headers[0].1, "mx.example.net; spf=pass");
        assert!(read[0].in_junk);
        assert_eq!(read[0].mail_text(100).text, "Guten Tag");

        assert!(foreign_mails(&json!([{}, {}]), 1..=1).is_err());
        assert!(foreign_mails(&json!([]), 1..=20).is_err());
        assert!(foreign_mails(&json!([{ "subject": "x".repeat(999) }]), 1..=1).is_err());
        assert!(foreign_mails(&json!([{ "date": "yesterday" }]), 1..=1).is_err());
        assert!(foreign_mails(&json!([{ "to": [{ "email": 5 }] }]), 1..=1).is_err());
        // Trace headers up to 16,000 characters, others up to 2,000; longer ones are refused (R4 I-4).
        let header = |name: &str, chars: usize| json!([{ "headers": [{ "name": name, "value": "v".repeat(chars) }] }]);
        for name in ["Authentication-Results", "received", "X-Spam-Status"] {
            assert_eq!(foreign_mails(&header(name, 16_000), 1..=1).unwrap()[0].headers[0].1.len(), 16_000);
            assert!(foreign_mails(&header(name, 16_001), 1..=1).is_err(), "{name}");
        }
        assert!(foreign_mails(&header("X-Other", 2000), 1..=1).is_ok());
        assert!(foreign_mails(&header("X-Other", 2001), 1..=1).is_err());
        let many: Vec<Value> = (0..51).map(|_| json!({ "email": "a@example.com" })).collect();
        assert!(foreign_mails(&json!([{ "cc": many }]), 1..=1).is_err());

        let labels = foreign_labels(&json!([
            { "name": " Rechnungen ", "description": "Rechnungen", "isSet": true },
            { "name": "Reisen" }
        ]))
        .unwrap();
        assert_eq!(
            labels[0],
            ForeignLabel { name: "Rechnungen".into(), description: "Rechnungen".into(), is_set: true }
        );
        assert!(foreign_labels(&json!([{ "name": "A" }, { "name": "a" }])).is_err());
        assert!(foreign_labels(&json!([{ "name": "" }])).is_err());
        assert!(foreign_labels(&json!([{ "name": "x".repeat(41) }])).is_err());
    }
}
