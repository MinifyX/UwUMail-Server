//! A label's own rules: conditions on sender, subject, text and attachments (docs/labels.md,
//! "Rules").

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Mail;
use crate::text::fold;

/// Conditions one label may have.
pub const MAX_CONDITIONS: usize = 10;
/// Characters of one condition's value, at most.
pub const MAX_VALUE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    From,
    Subject,
    Text,
    HasAttachment,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Field::From => "from",
            Field::Subject => "subject",
            Field::Text => "text",
            Field::HasAttachment => "hasAttachment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    pub field: Field,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Match {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    #[serde(rename = "match")]
    pub match_: Match,
    pub conditions: Vec<Condition>,
}

impl Rules {
    /// Checks rules as a client sends them (`{ match, conditions: [{ field, value }] }`), strictly:
    /// unknown keys, fields and values are refused with a sentence why. `null` and rules without
    /// conditions are `None`. Values come back trimmed.
    pub fn check(value: &Value) -> Result<Option<Rules>, String> {
        let object = match value {
            Value::Null => return Ok(None),
            Value::Object(object) => object,
            _ => return Err("rules are an object or null".into()),
        };
        if let Some(key) = object.keys().find(|key| !matches!(key.as_str(), "match" | "conditions")) {
            return Err(format!("rules have no property {key}"));
        }
        let match_ = match object.get("match") {
            None | Some(Value::Null) => Match::All,
            Some(Value::String(text)) if text == "all" => Match::All,
            Some(Value::String(text)) if text == "any" => Match::Any,
            Some(_) => return Err("match is \"all\" or \"any\"".into()),
        };
        let list = match object.get("conditions") {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::Array(list)) => list,
            Some(_) => return Err("conditions are a list".into()),
        };
        if list.len() > MAX_CONDITIONS {
            return Err(format!("a label has at most {MAX_CONDITIONS} conditions"));
        }
        let mut conditions = Vec::new();
        for item in list {
            let Value::Object(item) = item else { return Err("a condition is an object".into()) };
            if let Some(key) = item.keys().find(|key| !matches!(key.as_str(), "field" | "value")) {
                return Err(format!("a condition has no property {key}"));
            }
            let field = match item.get("field").and_then(Value::as_str) {
                Some("from") => Field::From,
                Some("subject") => Field::Subject,
                Some("text") => Field::Text,
                Some("hasAttachment") => Field::HasAttachment,
                _ => return Err("a condition's field is from, subject, text or hasAttachment".into()),
            };
            let value = item.get("value").and_then(Value::as_str).map(str::trim).unwrap_or_default();
            let chars = value.chars().count();
            if chars == 0 || chars > MAX_VALUE_CHARS || value.chars().any(char::is_control) {
                return Err(format!(
                    "a condition's value has 1 to {MAX_VALUE_CHARS} characters, without control characters"
                ));
            }
            if field == Field::HasAttachment && !matches!(value, "true" | "false") {
                return Err("hasAttachment takes \"true\" or \"false\"".into());
            }
            conditions.push(Condition { field, value: value.to_owned() });
        }
        Ok((!conditions.is_empty()).then_some(Rules { match_, conditions }))
    }

    /// The rules as JSON, as clients see them.
    pub fn to_json(&self) -> Value {
        json!(self)
    }

    /// The conditions that matched when the rules match `mail`, `None` otherwise.
    pub fn matches(&self, mail: &Mail) -> Option<Vec<Condition>> {
        let folded_subject = std::cell::OnceCell::new();
        let folded_text = std::cell::OnceCell::new();
        let mut matched = Vec::new();
        for condition in &self.conditions {
            let value = fold(&condition.value);
            let hit = match condition.field {
                Field::From => from_matches(&mail.from, &value),
                Field::Subject => folded_subject.get_or_init(|| fold(&mail.subject)).contains(&value),
                Field::Text => folded_text.get_or_init(|| fold(&mail.text)).contains(&value),
                Field::HasAttachment => (value == "true") == mail.has_attachment,
            };
            match (hit, self.match_) {
                (true, _) => matched.push(condition.clone()),
                (false, Match::All) => return None,
                (false, Match::Any) => {}
            }
        }
        (!matched.is_empty()).then_some(matched)
    }
}

/// `from` (lower case) against a condition's folded value: an address, or a domain with its
/// subdomains.
fn from_matches(from: &str, value: &str) -> bool {
    if value.find('@').is_some_and(|at| at > 0) {
        return from == value;
    }
    let domain = value.trim_start_matches('@');
    let Some((_, from_domain)) = from.rsplit_once('@') else { return false };
    !domain.is_empty()
        && (from_domain == domain || from_domain.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senders_are_addresses_or_domains() {
        assert!(from_matches("leni@example.org", "leni@example.org"));
        assert!(!from_matches("a.leni@example.org", "leni@example.org"));
        assert!(from_matches("x@mail.example.org", "example.org"));
        assert!(from_matches("x@example.org", "@example.org"));
        assert!(!from_matches("x@badexample.org", "example.org"));
    }
}
