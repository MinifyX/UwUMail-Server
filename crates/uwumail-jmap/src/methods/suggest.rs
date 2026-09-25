//! `AddressSuggestion/query` (docs/jmap-suggest.md): addresses to offer while someone types a
//! recipient, from their address books and the mail they recently sent and received, best first.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::Ctx;
use crate::error::{MethodError, MethodResult};
use crate::{dates, jscontact};

pub const DEFAULT_LIMIT: usize = 10;
pub const MAX_LIMIT: usize = 50;
/// The longest `text` looked for.
const MAX_TEXT_CHARS: usize = 256;
/// How many recent messages are looked through.
const MAX_MESSAGES: usize = 2000;

#[derive(Default)]
struct Candidate {
    email: String,
    name: String,
    contact: bool,
    sent: u32,
    received: u32,
    last_at: Option<i64>,
}

impl Candidate {
    fn source(&self) -> &'static str {
        if self.contact {
            "contact"
        } else if self.sent > 0 {
            "sent"
        } else {
            "received"
        }
    }

    fn sources(&self) -> Vec<&'static str> {
        let mut sources = Vec::new();
        if self.contact {
            sources.push("contact");
        }
        if self.sent > 0 {
            sources.push("sent");
        }
        if self.received > 0 {
            sources.push("received");
        }
        sources
    }

    /// How well the address and name fit what was typed: 3 the address or a word of the name
    /// starts with it, 2 the domain does, 1 it is somewhere inside, 0 not at all.
    fn fit(&self, text: &str) -> u8 {
        if text.is_empty() {
            return 1;
        }
        let email = self.email.to_lowercase();
        let name = self.name.to_lowercase();
        if email.starts_with(text)
            || name.starts_with(text)
            || name.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(text))
        {
            3
        } else if email.split_once('@').is_some_and(|(_, domain)| domain.starts_with(text)) {
            2
        } else if email.contains(text) || name.contains(text) {
            1
        } else {
            0
        }
    }

    /// People in the address book first, then whom one writes to, then who writes; the more and
    /// the more recent, the better.
    fn score(&self, now: i64) -> f64 {
        let recency = self.last_at.map_or(0.0, |at| {
            let days = ((now - at).max(0) as f64) / 86_400.0;
            30.0 / (1.0 + days / 7.0)
        });
        let contact = if self.contact { 40.0 } else { 0.0 };
        contact + 4.0 * f64::from(self.sent.min(50)) + f64::from(self.received.min(50)) + recency
    }
}

/// The name and addresses on a JSContact card.
fn card_addresses(card: &Map<String, Value>) -> (String, Vec<String>) {
    let name = card
        .get("name")
        .and_then(|name| {
            name.get("full").and_then(Value::as_str).map(str::to_owned).or_else(|| {
                let parts: Vec<&str> = name
                    .get("components")
                    .and_then(Value::as_array)?
                    .iter()
                    .filter_map(|component| component.get("value").and_then(Value::as_str))
                    .collect();
                (!parts.is_empty()).then(|| parts.join(" "))
            })
        })
        .unwrap_or_default();
    let emails = card
        .get("emails")
        .and_then(Value::as_object)
        .map(|emails| {
            emails.values().filter_map(|email| email.get("address").and_then(Value::as_str)).map(str::to_owned).collect()
        })
        .unwrap_or_default();
    (name, emails)
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let text = match args.get("text") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) if text.chars().count() <= MAX_TEXT_CHARS => text.trim().to_lowercase(),
        Some(Value::String(_)) => {
            return Err(MethodError::invalid_arguments(format!("text may have at most {MAX_TEXT_CHARS} characters")));
        }
        Some(_) => return Err(MethodError::invalid_arguments("text must be a string")),
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(value) => match value.as_u64() {
            Some(limit) if limit > 0 => (limit as usize).min(MAX_LIMIT),
            _ => return Err(MethodError::invalid_arguments("limit must be a positive number")),
        },
    };
    let store = &ctx.jmap.store;
    let own: Vec<String> = store.identities(ctx.account.id).await?.into_iter().map(|i| i.email.to_lowercase()).collect();
    let mut candidates: HashMap<String, Candidate> = HashMap::new();

    // The address books, whether or not the account uses CardDAV: they are its own.
    for record in store.contact_cards(ctx.account.id, None).await? {
        let Some(card) = jscontact::from_vcard(&record.content) else { continue };
        let (name, emails) = card_addresses(&card);
        for email in emails {
            let key = email.trim().to_lowercase();
            if key.is_empty() || !key.contains('@') {
                continue;
            }
            let candidate = candidates.entry(key).or_insert_with(|| Candidate { email: email.trim().to_owned(), ..Candidate::default() });
            candidate.contact = true;
            if !name.is_empty() {
                candidate.name = name.clone();
            }
        }
    }
    for used in store.address_history(ctx.account.id, &text, MAX_MESSAGES).await? {
        let key = used.address.email.trim().to_lowercase();
        if key.is_empty() || !key.contains('@') {
            continue;
        }
        let candidate = candidates
            .entry(key)
            .or_insert_with(|| Candidate { email: used.address.email.trim().to_owned(), ..Candidate::default() });
        if used.sent {
            candidate.sent += 1;
        } else {
            candidate.received += 1;
        }
        candidate.last_at = Some(candidate.last_at.map_or(used.at, |at| at.max(used.at)));
        // The address book's name wins; otherwise the latest name the address went by.
        if !candidate.contact
            && candidate.name.is_empty()
            && let Some(name) = used.address.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
        {
            candidate.name = name.to_owned();
        }
    }

    let now = super::unix_now();
    let mut ranked: Vec<(u8, f64, Candidate)> = candidates
        .into_iter()
        .filter(|(key, _)| !own.contains(key))
        .map(|(_, candidate)| (candidate.fit(&text), candidate.score(now), candidate))
        .filter(|(fit, _, _)| *fit > 0)
        .collect();
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then_with(|| a.2.email.to_lowercase().cmp(&b.2.email.to_lowercase()))
    });
    let list: Vec<Value> = ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, candidate)| {
            json!({
                "email": candidate.email,
                "name": if candidate.name.is_empty() { Value::Null } else { json!(candidate.name) },
                "source": candidate.source(),
                "sources": candidate.sources(),
                "lastUsedAt": candidate.last_at.map(dates::format),
            })
        })
        .collect();
    Ok(json!({ "accountId": ctx.account_id(), "list": list }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_fit_better_than_the_middle() {
        let candidate =
            Candidate { email: "nyu.katze@cats.example".into(), name: "Nyu Katze".into(), ..Candidate::default() };
        assert_eq!(candidate.fit("kat"), 3);
        assert_eq!(candidate.fit("nyu"), 3);
        assert_eq!(candidate.fit("cats"), 2);
        assert_eq!(candidate.fit("atze"), 1);
        assert_eq!(candidate.fit("hund"), 0);
        assert_eq!(candidate.fit(""), 1);
    }

    #[test]
    fn contacts_and_recent_recipients_come_first() {
        let now = 1_800_000_000;
        let contact = Candidate { contact: true, ..Candidate::default() };
        let written_to = Candidate { sent: 3, last_at: Some(now - 86_400), ..Candidate::default() };
        let heard_from = Candidate { received: 3, last_at: Some(now - 86_400), ..Candidate::default() };
        let long_ago = Candidate { received: 3, last_at: Some(now - 400 * 86_400), ..Candidate::default() };
        assert!(contact.score(now) > heard_from.score(now));
        assert!(written_to.score(now) > heard_from.score(now));
        assert!(heard_from.score(now) > long_ago.score(now));
        assert_eq!(contact.source(), "contact");
        assert_eq!(written_to.sources(), ["sent"]);
    }
}
