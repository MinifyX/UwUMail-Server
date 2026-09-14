use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};

/// A mailbox address with an optional display name, as stored in message metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    pub email: String,
}

/// Lowercases a domain and converts international names to their ASCII form.
pub fn normalize_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return Err(StoreError::Invalid("the domain is empty".into()));
    }
    let ascii = idna::domain_to_ascii(domain)
        .map_err(|_| StoreError::Invalid(format!("'{domain}' is not a valid domain name")))?;
    let valid = ascii.len() <= 253
        && ascii.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if !valid {
        return Err(StoreError::Invalid(format!("'{domain}' is not a valid domain name")));
    }
    Ok(ascii)
}

/// Splits and normalizes `local@domain`. Local parts are compared case-insensitively.
pub fn normalize_address(address: &str) -> Result<(String, String)> {
    let address = address.trim().trim_start_matches('<').trim_end_matches('>');
    let (local, domain) =
        address.rsplit_once('@').ok_or_else(|| StoreError::Invalid(format!("'{address}' is not an email address")))?;
    let local = local.to_lowercase();
    let valid_local = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.chars().all(|c| !c.is_whitespace() && !c.is_control() && !"<>()[]\\,;:\"@".contains(c));
    if !valid_local {
        return Err(StoreError::Invalid(format!("'{address}' is not a valid email address")));
    }
    Ok((local, normalize_domain(domain)?))
}

/// Strips a `+tag` sub-address from a local part.
pub(crate) fn base_local_part(local: &str) -> &str {
    match local.split_once('+') {
        Some((base, _)) if !base.is_empty() => base,
        _ => local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_addresses() {
        assert_eq!(normalize_address(" <Mini@Example.DE> ").unwrap(), ("mini".into(), "example.de".into()));
        assert_eq!(normalize_address("nyu@bücher.de").unwrap(), ("nyu".into(), "xn--bcher-kva.de".into()));
        assert!(normalize_address("no-at-sign").is_err());
        assert!(normalize_address("a b@example.de").is_err());
        assert!(normalize_address("mini@-bad-.de").is_err());
    }

    #[test]
    fn strips_sub_addresses() {
        assert_eq!(base_local_part("mini+shop"), "mini");
        assert_eq!(base_local_part("+only"), "+only");
        assert_eq!(base_local_part("plain"), "plain");
    }
}
