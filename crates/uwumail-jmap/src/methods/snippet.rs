//! SearchSnippet/get (RFC 8621, section 5): subject and preview with search words marked.

use serde_json::{Value, json};

use super::Ctx;
use crate::error::{MethodError, MethodResult};

fn search_terms(filter: &Value, out: &mut Vec<String>) {
    if let Some(conditions) = filter.get("conditions").and_then(Value::as_array) {
        if filter.get("operator").and_then(Value::as_str) != Some("NOT") {
            conditions.iter().for_each(|c| search_terms(c, out));
        }
        return;
    }
    for key in ["text", "subject", "body"] {
        if let Some(text) = filter.get(key).and_then(Value::as_str) {
            out.extend(text.split_whitespace().map(|t| t.trim_matches('"').to_lowercase()).filter(|t| !t.is_empty()));
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// HTML-escaped text with every term wrapped in `<mark>`, or `None` if nothing matched.
fn highlight(text: &str, terms: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    // Lowercasing can change byte lengths; only highlight when positions line up.
    if lower.len() != text.len() || terms.is_empty() {
        return None;
    }
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let mut from = 0;
        while let Some(found) = lower[from..].find(term.as_str()) {
            let start = from + found;
            marks.push((start, start + term.len()));
            from = start + term.len();
        }
    }
    if marks.is_empty() {
        return None;
    }
    marks.sort_unstable();
    let mut out = String::new();
    let mut position = 0;
    for (start, end) in marks {
        if start < position || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        out.push_str(&escape(&text[position..start]));
        out.push_str("<mark>");
        out.push_str(&escape(&text[start..end]));
        out.push_str("</mark>");
        position = end;
    }
    out.push_str(&escape(&text[position..]));
    Some(out)
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let email_ids: Vec<String> = args
        .get("emailIds")
        .and_then(Value::as_array)
        .ok_or_else(|| MethodError::invalid_arguments("emailIds is required"))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let mut terms = Vec::new();
    if let Some(filter) = args.get("filter") {
        search_terms(filter, &mut terms);
    }
    let numbers: Vec<i64> = email_ids.iter().filter_map(|id| ctx.parse_id('e', id)).collect();
    let records = ctx.jmap.store.emails_by_ids(ctx.account.id, numbers).await?;
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in email_ids {
        match ctx.parse_id('e', &id).and_then(|n| records.iter().find(|r| r.id == n)) {
            Some(record) => list.push(json!({
                "emailId": id,
                "subject": highlight(&record.subject, &terms),
                "preview": highlight(&record.preview, &terms),
            })),
            None => not_found.push(id),
        }
    }
    Ok(
        json!({ "accountId": ctx.account_id(), "list": list, "notFound": if not_found.is_empty() { Value::Null } else { json!(not_found) } }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_terms() {
        assert_eq!(
            highlight("Katzenfutter <3", &["futter".into()]).as_deref(),
            Some("Katzen<mark>futter</mark> &lt;3")
        );
        assert_eq!(highlight("Hallo", &["tschüss".into()]), None);
        let mut terms = Vec::new();
        search_terms(
            &json!({ "operator": "AND", "conditions": [{ "text": "Nyu" }, { "inMailbox": "m1" }] }),
            &mut terms,
        );
        assert_eq!(terms, vec!["nyu"]);
    }
}
