//! Sender Rewriting Scheme for forwarded mail. The envelope sender becomes an address on our
//! domain, so SPF passes at the next server, and bounces come back to us to be passed on.
//!
//! `SRS0=hash=tt=example.net=leni@our.domain`: `tt` is the day (two base32 characters), `hash`
//! an HMAC over day, domain and local part, in lower case because some servers change the case
//! of local parts.

use aws_lc_rs::hmac;
use uwumail_store::Store;

use crate::now;

const SECRET_KEY: &str = "srs.secret";
/// Bounces to a rewritten address are accepted this many days after the forward.
const MAX_AGE_DAYS: i64 = 21;
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The server's SRS secret, created on first use.
pub(crate) async fn secret(store: &Store) -> Option<Vec<u8>> {
    if let Ok(Some(value)) = store.setting(SECRET_KEY).await
        && let Ok(bytes) = hex::decode(value)
    {
        return Some(bytes);
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system RNG failed");
    store.set_setting(SECRET_KEY, &hex::encode(bytes)).await.ok()?;
    Some(bytes.to_vec())
}

fn day_tag(day: i64) -> String {
    let day = day.rem_euclid(1024) as usize;
    [BASE32[day >> 5] as char, BASE32[day & 31] as char].into_iter().collect()
}

fn day_from_tag(tag: &str, today: i64) -> Option<i64> {
    let bytes = tag.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let value = |b: u8| BASE32.iter().position(|c| *c == b.to_ascii_lowercase()).map(|p| p as i64);
    let tagged = value(bytes[0])? * 32 + value(bytes[1])?;
    // The tag wraps every 1024 days; pick the most recent day with that tag.
    Some(today - (today - tagged).rem_euclid(1024))
}

fn hash(secret: &[u8], tag: &str, domain: &str, local: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let data = format!("{tag}={domain}={local}").to_ascii_lowercase();
    let tag = hmac::sign(&key, data.as_bytes());
    hex::encode(&tag.as_ref()[..4])
}

/// Rewrites `sender` for a forward from `our_domain`. Empty senders (bounces) stay empty.
pub(crate) fn rewrite(secret: &[u8], sender: &str, our_domain: &str) -> String {
    let Some((local, domain)) = sender.rsplit_once('@') else {
        return sender.to_owned();
    };
    let tag = day_tag(now().div_euclid(86_400));
    format!("SRS0={}={tag}={domain}={local}@{our_domain}", hash(secret, &tag, domain, local))
}

/// The original sender behind a rewritten address, if it is ours, intact and recent.
pub(crate) fn reverse(secret: &[u8], address: &str) -> Option<String> {
    let (local, _) = address.rsplit_once('@')?;
    if !looks_like_srs(local) {
        return None;
    }
    let mut parts = local[5..].splitn(4, '=');
    let (hash_part, tag, domain, original_local) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if original_local.is_empty() || domain.is_empty() {
        return None;
    }
    let expected = hash(secret, tag, domain, original_local);
    let valid = aws_lc_rs::constant_time::verify_slices_are_equal(
        expected.as_bytes(),
        hash_part.to_ascii_lowercase().as_bytes(),
    )
    .is_ok();
    let today = now().div_euclid(86_400);
    let day = day_from_tag(tag, today)?;
    (valid && today - day <= MAX_AGE_DAYS).then(|| format!("{original_local}@{domain}"))
}

pub(crate) fn looks_like_srs(address: &str) -> bool {
    address.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case("srs0="))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewritten_senders_come_back_intact() {
        let secret = b"a test secret that is long enough";
        let rewritten = rewrite(secret, "Oma.Heinz@example.net", "example.org");
        assert!(rewritten.starts_with("SRS0=") && rewritten.ends_with("=example.net=Oma.Heinz@example.org"));
        assert_eq!(reverse(secret, &rewritten).as_deref(), Some("Oma.Heinz@example.net"));
        assert_eq!(reverse(secret, &rewritten.to_ascii_lowercase()).as_deref(), Some("oma.heinz@example.net"));
        assert_eq!(reverse(b"another secret", &rewritten), None, "forged");
        assert_eq!(reverse(secret, "SRS0=0000=aa=example.net=x@example.org"), None);
        assert_eq!(reverse(secret, "leni@example.org"), None);
        assert_eq!(rewrite(secret, "", "example.org"), "");
    }

    #[test]
    fn day_tags_wrap_and_expire() {
        let today = 20_710;
        assert_eq!(day_from_tag(&day_tag(today), today), Some(today));
        assert_eq!(day_from_tag(&day_tag(today - 3), today), Some(today - 3));
        assert_eq!(day_from_tag(&day_tag(today + 1), today), Some(today + 1 - 1024), "tomorrow reads as long ago");
    }
}
