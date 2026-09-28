//! Email/unsubscribe: one-click unsubscribing (RFC 8058) through the server, under the capability
//! `urn:uwumail:jmap:unsubscribe` (docs/jmap-unsubscribe.md).
//!
//! This is the one place where a header of a received message decides where the server sends a
//! request, so it only does when the sender vouched for that header: a DKIM signature that still holds
//! and covers both `List-Unsubscribe` and `List-Unsubscribe-Post`. The request itself leaves like a
//! remote picture — public addresses only, through `[egress]` — and says nothing but
//! `List-Unsubscribe=One-Click`.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uwumail_smtp::egress::EgressError;

use super::Ctx;
use super::email::visible_records;
use crate::error::{MethodError, MethodResult};

/// The same email is sent at most once in this long, whatever came of it.
const EMAIL_WINDOW: Duration = Duration::from_secs(5 * 60);
/// Unsubscriptions one login may send in [`ACCOUNT_WINDOW`], in its own and in shared accounts.
const ACCOUNT_BUDGET: usize = 30;
const ACCOUNT_WINDOW: Duration = Duration::from_secs(3600);
/// Signature checks one login may start in [`ACCOUNT_WINDOW`], whether or not they lead anywhere:
/// each one reads and hashes a whole message and asks DNS.
const CHECK_BUDGET: usize = 120;
/// Emails remembered before the ones out of their window are swept out.
const MAX_REMEMBERED: usize = 10_000;
/// Longer links are not followed; real ones are a few hundred characters.
const MAX_LINK_LENGTH: usize = 2048;
/// How long checking the signatures again may take, DNS lookups included.
const DKIM_TIMEOUT: Duration = Duration::from_secs(10);
const ONE_CLICK: &str = "List-Unsubscribe=One-Click";

/// Sends the POST of a one-click unsubscription and answers the HTTP status. The server's own is its
/// egress ([`uwumail_smtp::egress::Egress::unsubscribe`]); tests hand in one that reaches a newsletter on
/// the same machine.
pub trait UnsubscribeTransport: Send + Sync + 'static {
    fn post<'a>(&'a self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<u16, EgressError>> + Send + 'a>>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sending,
    Succeeded,
    Failed,
}

#[derive(Default)]
struct Seen {
    /// (owner account, email) → when it was sent and what came of it.
    emails: HashMap<(i64, i64), (Instant, Outcome)>,
    /// Login → when it sent its latest unsubscriptions.
    logins: HashMap<i64, VecDeque<Instant>>,
    /// Login → when it had signatures checked lately.
    checks: HashMap<i64, VecDeque<Instant>>,
}

/// Counts one more use in a sliding window; false when `budget` is used up.
fn spend(times: &mut VecDeque<Instant>, now: Instant, budget: usize) -> bool {
    while times.front().is_some_and(|at| now.duration_since(*at) >= ACCOUNT_WINDOW) {
        times.pop_front();
    }
    if times.len() >= budget {
        return false;
    }
    times.push_back(now);
    true
}

/// What was sent lately, for the limits. Kept in memory: a restart forgets it, which costs at most one
/// more request per email.
#[derive(Default)]
pub struct Unsubscribes {
    seen: Mutex<Seen>,
}

enum Turn {
    Go,
    /// Sent a moment ago and it worked: nothing to do again.
    AlreadyDone,
    Wait(String),
}

impl Unsubscribes {
    /// Whether a login may have one more message's signatures checked this hour.
    fn may_check(&self, login: i64) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if seen.checks.len() >= MAX_REMEMBERED {
            seen.checks.retain(|_, times| times.back().is_some_and(|at| now.duration_since(*at) < ACCOUNT_WINDOW));
        }
        spend(seen.checks.entry(login).or_default(), now, CHECK_BUDGET)
    }

    fn begin(&self, owner: i64, email: i64, login: i64) -> Turn {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if seen.emails.len() >= MAX_REMEMBERED {
            seen.emails.retain(|_, (at, _)| now.duration_since(*at) < EMAIL_WINDOW);
            seen.logins.retain(|_, times| times.back().is_some_and(|at| now.duration_since(*at) < ACCOUNT_WINDOW));
        }
        if let Some((at, outcome)) = seen.emails.get(&(owner, email))
            && now.duration_since(*at) < EMAIL_WINDOW
        {
            let minutes = (EMAIL_WINDOW - now.duration_since(*at)).as_secs().div_ceil(60);
            return match outcome {
                Outcome::Succeeded => Turn::AlreadyDone,
                Outcome::Sending => Turn::Wait("this unsubscription is being sent right now".into()),
                Outcome::Failed => Turn::Wait(format!(
                    "this unsubscription was tried a moment ago; it can be tried again in {minutes} min"
                )),
            };
        }
        if !spend(seen.logins.entry(login).or_default(), now, ACCOUNT_BUDGET) {
            return Turn::Wait(format!("at most {ACCOUNT_BUDGET} unsubscriptions an hour; please wait a little"));
        }
        seen.emails.insert((owner, email), (now, Outcome::Sending));
        Turn::Go
    }

    fn finish(&self, owner: i64, email: i64, succeeded: bool) {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, outcome)) = seen.emails.get_mut(&(owner, email)) {
            *outcome = if succeeded { Outcome::Succeeded } else { Outcome::Failed };
        }
    }
}

fn cannot(description: impl Into<String>) -> MethodError {
    MethodError::new("cannotUnsubscribe", description)
}

fn failed(description: impl Into<String>) -> MethodError {
    MethodError::new("unsubscribeFailed", description)
}

pub async fn unsubscribe(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let requested = args
        .get("emailId")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_arguments("emailId is required"))?;
    let id = ctx.parse_id('e', requested).ok_or_else(|| MethodError::kind("notFound"))?;
    let store = &ctx.jmap.store;
    let record = visible_records(ctx, store.emails_by_ids(ctx.account.id, vec![id]).await?)
        .into_iter()
        .next()
        .ok_or_else(|| MethodError::kind("notFound"))?;
    // Unsubscribing acts for the owner: in a shared mailbox it takes the right to change its messages,
    // not only to read them.
    if let Some(view) = &ctx.shared
        && !record.mailbox_ids.iter().any(|mailbox| view.may(*mailbox, "w"))
    {
        return Err(MethodError::new("forbidden", "you may only read this message, not unsubscribe for its owner"));
    }
    let owner = ctx.account.id;
    let login = ctx.shared.as_ref().map_or(owner, |view| view.me.id);
    if !ctx.jmap.unsubscribes.may_check(login) {
        return Err(failed(format!(
            "at most {CHECK_BUDGET} messages checked for unsubscribing an hour; please wait a little"
        )));
    }
    let raw = store.blob(&record.blob).await?;
    let link = one_click_link(&raw).map_err(cannot)?;
    let signed = tokio::time::timeout(DKIM_TIMEOUT, ctx.jmap.smtp.dkim_signed_headers(&raw)).await.unwrap_or_default();
    if !covers_both(&signed) {
        return Err(cannot("no valid DKIM signature covers List-Unsubscribe and List-Unsubscribe-Post"));
    }

    let response = json!({ "accountId": ctx.account_id(), "emailId": requested });
    match ctx.jmap.unsubscribes.begin(owner, id, login) {
        Turn::Go => {}
        Turn::AlreadyDone => return Ok(response),
        Turn::Wait(why) => return Err(failed(why)),
    }
    let result = match &ctx.jmap.unsubscribe_transport {
        Some(transport) => transport.post(&link).await,
        None => ctx.jmap.egress.unsubscribe(&link).await,
    };
    let outcome = match result {
        Ok(status) if (200..300).contains(&status) => Ok(()),
        Ok(status) if (300..400).contains(&status) => Err(format!(
            "the sender's server answered with a redirect ({status}), which one-click unsubscribing does not follow"
        )),
        Ok(status) => Err(format!("the sender's server answered {status}")),
        Err(EgressError::NotAllowed(why)) => Err(format!("the unsubscribe link can't be used: {why}")),
        Err(EgressError::Timeout) => Err("the sender's server did not answer within 20 seconds".into()),
        Err(EgressError::Unreachable) if ctx.jmap.egress.proxied() => {
            Err("the sender's server could not be reached, or the egress proxy is away".into())
        }
        Err(EgressError::Unreachable) => Err("the sender's server could not be reached".into()),
        Err(err) => Err(err.to_string()),
    };
    ctx.jmap.unsubscribes.finish(owner, id, outcome.is_ok());
    // The link often carries a token that unsubscribes whoever has it: only its host goes to the log.
    let host = host_of(&link);
    match outcome {
        Ok(()) => {
            tracing::info!(account = login, owner, email = id, host, "one-click unsubscribe sent");
            Ok(response)
        }
        Err(why) => {
            tracing::info!(account = login, owner, email = id, host, reason = %why, "one-click unsubscribe failed");
            Err(failed(why))
        }
    }
}

/// The https link of a message that offers one-click unsubscribing, or why it doesn't. Each of the two
/// headers must be there exactly once: DKIM covers the last one of a name, and a second one written
/// above it in transit would go unsigned.
fn one_click_link(raw: &[u8]) -> Result<String, &'static str> {
    let post = uwumail_smtp::header_values(raw, "List-Unsubscribe-Post");
    let links = uwumail_smtp::header_values(raw, "List-Unsubscribe");
    match (post.as_slice(), links.as_slice()) {
        ([], _) | (_, []) => Err("the message does not offer one-click unsubscribing"),
        ([post], [links]) => {
            if !post.trim().eq_ignore_ascii_case(ONE_CLICK) {
                return Err("List-Unsubscribe-Post does not say List-Unsubscribe=One-Click");
            }
            https_link(links).ok_or("List-Unsubscribe has no usable https link")
        }
        _ => Err("the message has more than one List-Unsubscribe or List-Unsubscribe-Post header"),
    }
}

/// The first `<https:…>` in a `List-Unsubscribe` value (RFC 2369: bracketed, comma-separated;
/// whitespace inside the brackets is ignored).
fn https_link(value: &str) -> Option<String> {
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let end = start + rest[start..].find('>')?;
        let link: String = rest[start + 1..end].chars().filter(|c| !c.is_whitespace()).collect();
        rest = &rest[end + 1..];
        let https = link.get(..6).is_some_and(|scheme| scheme.eq_ignore_ascii_case("https:"));
        if https && link.len() <= MAX_LINK_LENGTH && link.bytes().all(|b| b.is_ascii_graphic()) {
            return Some(link);
        }
    }
    None
}

fn covers_both(signed: &[Vec<String>]) -> bool {
    let has = |headers: &[String], name: &str| headers.iter().any(|header| header.trim().eq_ignore_ascii_case(name));
    signed.iter().any(|headers| has(headers, "List-Unsubscribe") && has(headers, "List-Unsubscribe-Post"))
}

/// The host of an https link, for the log.
fn host_of(link: &str) -> &str {
    let rest = link.get(8..).unwrap_or_default();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit('@').next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(headers: &str) -> Vec<u8> {
        format!("From: news@shop.example\r\n{headers}Subject: Hi\r\n\r\nBody\r\n").into_bytes()
    }

    #[test]
    fn only_a_single_https_link_with_the_one_click_post_counts() {
        let good = message(
            "List-Unsubscribe: <mailto:u@shop.example>,\r\n <https://shop.example/u/\r\n abc?t=1>\r\n\
             List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
        );
        assert_eq!(one_click_link(&good).unwrap(), "https://shop.example/u/abc?t=1");
        let plain = message("List-Unsubscribe: <https://shop.example/u>\r\n");
        assert!(one_click_link(&plain).is_err(), "no List-Unsubscribe-Post");
        let http = message(
            "List-Unsubscribe: <http://shop.example/u>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
        );
        assert!(one_click_link(&http).is_err(), "https only");
        let other = message("List-Unsubscribe: <https://shop.example/u>\r\nList-Unsubscribe-Post: Something=Else\r\n");
        assert!(one_click_link(&other).is_err());
        let twice = message(
            "List-Unsubscribe: <https://evil.example/u>\r\nList-Unsubscribe: <https://shop.example/u>\r\n\
             List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n",
        );
        assert!(one_click_link(&twice).is_err(), "an unsigned second header could be the one taken");
        let long = format!("<https://shop.example/{}>", "a".repeat(MAX_LINK_LENGTH));
        assert_eq!(https_link(&long), None);
        assert_eq!(https_link("<HTTPS://shop.example/u>"), Some("HTTPS://shop.example/u".into()));
        assert_eq!(https_link("https://shop.example/u"), None, "only bracketed links");
    }

    #[test]
    fn a_signature_must_cover_both_headers() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert!(covers_both(&[names(&["from", "list-unsubscribe", "List-Unsubscribe-Post"])]));
        assert!(!covers_both(&[names(&["from", "list-unsubscribe"]), names(&["list-unsubscribe-post"])]));
        assert!(!covers_both(&[]));
    }

    #[test]
    fn only_the_host_is_logged() {
        assert_eq!(host_of("https://shop.example/u/secret-token"), "shop.example");
        assert_eq!(host_of("https://shop.example:8443?t=1"), "shop.example:8443");
    }

    #[test]
    fn each_email_once_in_a_while_and_a_budget_per_login() {
        let limits = Unsubscribes::default();
        assert!(matches!(limits.begin(1, 10, 1), Turn::Go));
        assert!(matches!(limits.begin(1, 10, 1), Turn::Wait(_)), "still being sent");
        limits.finish(1, 10, true);
        assert!(matches!(limits.begin(1, 10, 1), Turn::AlreadyDone));
        assert!(matches!(limits.begin(1, 11, 1), Turn::Go));
        limits.finish(1, 11, false);
        assert!(matches!(limits.begin(1, 11, 1), Turn::Wait(_)), "a failure is not tried again at once");
        for email in 12..(12 + ACCOUNT_BUDGET as i64 - 2) {
            assert!(matches!(limits.begin(1, email, 1), Turn::Go));
        }
        assert!(matches!(limits.begin(1, 999, 1), Turn::Wait(_)), "the hour's budget is used up");
        assert!(matches!(limits.begin(2, 999, 2), Turn::Go), "someone else's is not");
    }

    #[test]
    fn signature_checks_have_a_budget_per_login() {
        let limits = Unsubscribes::default();
        for _ in 0..CHECK_BUDGET {
            assert!(limits.may_check(1));
        }
        assert!(!limits.may_check(1), "a login cannot have messages hashed without end");
        assert!(limits.may_check(2));
    }
}
