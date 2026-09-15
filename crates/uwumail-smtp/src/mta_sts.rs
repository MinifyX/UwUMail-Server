//! MTA-STS (RFC 8461): the policy our domains publish, and the policies of the domains we
//! deliver to, which say that their MX hosts must be reached over TLS with a valid certificate.

use std::time::Duration;

use aws_lc_rs::digest;
use mail_auth::mta_sts::MtaSts;
use uwumail_store::CachedStsPolicy;

use crate::{Context, now};

/// How long senders keep our policy: short while testing, a week once it is enforced.
pub const TESTING_MAX_AGE: u64 = 24 * 3600;
pub const ENFORCE_MAX_AGE: u64 = 7 * 24 * 3600;
/// RFC 8461 allows at most one year.
const MAX_AGE_LIMIT: u64 = 31_557_600;
const MAX_POLICY_BYTES: usize = 64 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Enforce,
    Testing,
    None,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Enforce => "enforce",
            Mode::Testing => "testing",
            Mode::None => "none",
        }
    }

    fn parse(value: &str) -> Option<Mode> {
        match value {
            "enforce" => Some(Mode::Enforce),
            "testing" => Some(Mode::Testing),
            "none" => Some(Mode::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    /// MX names, possibly with a leading `*.` for exactly one label.
    pub mx: Vec<String>,
    pub max_age: u64,
}

impl Policy {
    /// The policy for one of our domains.
    pub fn ours(mode: uwumail_store::MtaStsMode, mx: &[String]) -> Policy {
        let (mode, max_age) = match mode {
            uwumail_store::MtaStsMode::Testing => (Mode::Testing, TESTING_MAX_AGE),
            uwumail_store::MtaStsMode::Enforce => (Mode::Enforce, ENFORCE_MAX_AGE),
        };
        Policy { mode, mx: mx.iter().map(|name| name.trim_end_matches('.').to_ascii_lowercase()).collect(), max_age }
    }

    pub fn parse(text: &str) -> Result<Policy, String> {
        let mut version = None;
        let mut mode = None;
        let mut mx = Vec::new();
        let mut max_age = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                return Err(format!("the line \"{line}\" is not key: value"));
            };
            let value = value.trim();
            match key.trim() {
                "version" => version = Some(value.to_owned()),
                "mode" => mode = Some(Mode::parse(value).ok_or_else(|| format!("unknown mode \"{value}\""))?),
                "mx" => mx.push(value.trim_end_matches('.').to_ascii_lowercase()),
                "max_age" => {
                    let seconds: u64 = value.parse().map_err(|_| format!("max_age \"{value}\" is not a number"))?;
                    max_age = Some(seconds.min(MAX_AGE_LIMIT));
                }
                _ => {}
            }
        }
        if version.as_deref() != Some("STSv1") {
            return Err("the policy does not start with version: STSv1".into());
        }
        let mode = mode.ok_or("the policy has no mode")?;
        let max_age = max_age.ok_or("the policy has no max_age")?;
        if mode != Mode::None && mx.is_empty() {
            return Err("the policy lists no mx".into());
        }
        Ok(Policy { mode, mx, max_age })
    }

    /// The file senders fetch from `https://mta-sts.<domain>/.well-known/mta-sts.txt`.
    pub fn to_text(&self) -> String {
        let mut text = format!("version: STSv1\r\nmode: {}\r\n", self.mode.as_str());
        for name in &self.mx {
            text.push_str(&format!("mx: {name}\r\n"));
        }
        text.push_str(&format!("max_age: {}\r\n", self.max_age));
        text
    }

    /// Changes whenever the policy does, which tells senders to fetch it again.
    pub fn id(&self) -> String {
        let hash = digest::digest(&digest::SHA256, self.to_text().as_bytes());
        hex::encode(&hash.as_ref()[..10])
    }

    /// Whether an MX host may be used under this policy.
    pub fn allows(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.mx.iter().any(|pattern| match pattern.strip_prefix("*.") {
            Some(parent) => host.split_once('.').is_some_and(|(label, rest)| !label.is_empty() && rest == parent),
            None => *pattern == host,
        })
    }
}

/// The TXT record that announces a policy.
pub fn txt_record(policy: &Policy) -> String {
    format!("v=STSv1; id={}", policy.id())
}

pub fn policy_url(domain: &str) -> String {
    format!("https://mta-sts.{}/.well-known/mta-sts.txt", domain.trim_end_matches('.').to_ascii_lowercase())
}

/// Checks a fetched policy file: text/plain and well-formed.
pub fn read_fetched(fetched: &crate::https::Fetched) -> Result<Policy, String> {
    let media_type = fetched.content_type.split(';').next().unwrap_or_default().trim();
    if !media_type.eq_ignore_ascii_case("text/plain") {
        return Err(format!("the policy is served as \"{media_type}\" instead of text/plain"));
    }
    Policy::parse(&fetched.body)
}

impl From<&CachedStsPolicy> for Policy {
    fn from(cached: &CachedStsPolicy) -> Policy {
        Policy {
            mode: Mode::parse(&cached.mode).unwrap_or(Mode::None),
            mx: cached.mx.clone(),
            max_age: cached.max_age.max(0) as u64,
        }
    }
}

/// The policy to follow when delivering to `domain`, or `None` when it has none.
///
/// A cached policy is used while its id matches the TXT record and it has not expired. When
/// fetching a new one fails, a cached policy that has not expired still applies (RFC 8461 §5.1).
pub(crate) async fn policy_for(ctx: &Context, domain: &str) -> Option<Policy> {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let cached = ctx.store.cached_sts_policy(&domain).await.ok().flatten().filter(|cached| !cached.expired(now()));
    let record = ctx.authenticator.txt_lookup::<MtaSts>(format!("_mta-sts.{domain}."), Some(&ctx.dns.txt)).await;
    let id = match record {
        Ok(record) => record.id.clone(),
        // No record: the domain has no policy, unless one we saw before is still valid.
        Err(_) => return cached.as_ref().map(Policy::from),
    };
    if let Some(cached) = &cached
        && cached.policy_id == id
    {
        return Some(Policy::from(cached));
    }
    match ctx.https.get(&policy_url(&domain), MAX_POLICY_BYTES, FETCH_TIMEOUT).await.and_then(|f| read_fetched(&f)) {
        Ok(policy) => {
            let entry = CachedStsPolicy {
                domain: domain.clone(),
                policy_id: id,
                mode: policy.mode.as_str().to_owned(),
                mx: policy.mx.clone(),
                max_age: policy.max_age as i64,
                fetched_at: now(),
            };
            if let Err(err) = ctx.store.cache_sts_policy(entry).await {
                tracing::warn!(%err, %domain, "caching an MTA-STS policy failed");
            }
            Some(policy)
        }
        Err(error) => {
            tracing::info!(%domain, %error, "fetching the MTA-STS policy failed");
            cached.as_ref().map(Policy::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::Fetched;

    #[test]
    fn our_policy_round_trips_and_its_id_follows_the_content() {
        let testing = Policy::ours(uwumail_store::MtaStsMode::Testing, &["Mail.Example.DE.".into()]);
        let text = testing.to_text();
        assert_eq!(text, "version: STSv1\r\nmode: testing\r\nmx: mail.example.de\r\nmax_age: 86400\r\n");
        assert_eq!(Policy::parse(&text).unwrap(), testing);

        let enforce = Policy::ours(uwumail_store::MtaStsMode::Enforce, &["mail.example.de".into()]);
        assert_ne!(testing.id(), enforce.id());
        assert_eq!(testing.id().len(), 20);
        assert!(testing.id().chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(txt_record(&testing), format!("v=STSv1; id={}", testing.id()));
    }

    #[test]
    fn policies_from_elsewhere_are_read_carefully() {
        let policy = Policy::parse(
            "version: STSv1\nmode: enforce\nmx: *.mx.example.net\nmx: mail.example.net.\nmax_age: 999999999\n",
        )
        .unwrap();
        assert_eq!(policy.mode, Mode::Enforce);
        assert_eq!(policy.max_age, 31_557_600, "capped at a year");
        assert!(policy.allows("a.mx.example.net"));
        assert!(policy.allows("MAIL.example.net."));
        assert!(!policy.allows("mx.example.net"), "the wildcard needs one label");
        assert!(!policy.allows("a.b.mx.example.net"), "and only one");
        assert!(!policy.allows("evil.example.org"));

        assert!(Policy::parse("mode: enforce\nmx: a.example\nmax_age: 60").is_err(), "version is required");
        assert!(Policy::parse("version: STSv1\nmode: enforce\nmax_age: 60").is_err(), "enforce needs mx");
        assert!(Policy::parse("version: STSv1\nmode: none\nmax_age: 60").is_ok());
        assert!(Policy::parse("version: STSv1\nmode: sometimes\nmx: a\nmax_age: 60").is_err());

        let fetched = |content_type: &str| Fetched {
            content_type: content_type.into(),
            body: "version: STSv1\nmode: testing\nmx: a.example\nmax_age: 60\n".into(),
        };
        assert!(read_fetched(&fetched("text/plain; charset=utf-8")).is_ok());
        assert!(read_fetched(&fetched("text/html")).is_err());
        assert_eq!(policy_url("Example.DE."), "https://mta-sts.example.de/.well-known/mta-sts.txt");
    }
}
