//! The SASL mechanisms mail apps sign in with an OAuth access token: OAUTHBEARER (RFC 7628) and
//! Google's older XOAUTH2, which many apps still speak. Only the parsing and the error answer live
//! here; IMAP, SMTP and ManageSieve each put them on their own wire.

/// What an app sent: the login it names, if any, and the token.
#[derive(Clone, PartialEq, Eq)]
pub struct SaslBearer {
    pub user: Option<String>,
    pub token: String,
}

impl std::fmt::Debug for SaslBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token never into a log line.
        f.debug_struct("SaslBearer").field("user", &self.user).finish_non_exhaustive()
    }
}

/// The token of an `auth=Bearer <token>` value.
fn bearer(value: &str) -> Option<String> {
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_owned())
}

/// OAUTHBEARER's client response: `n,a=user,` `^A` `auth=Bearer token` `^A` … `^A^A`.
pub fn parse_oauthbearer(message: &[u8]) -> Option<SaslBearer> {
    let message = std::str::from_utf8(message).ok()?;
    let (gs2, rest) = message.split_once('\x01')?;
    // The GS2 header: no channel binding ("n" or "y"), an optional authorization identity, and
    // nothing else.
    let mut parts = gs2.splitn(3, ',');
    let binding = parts.next()?;
    if !matches!(binding, "n" | "y") {
        return None;
    }
    let authzid = parts.next()?;
    let user = match authzid {
        "" => None,
        other => Some(decode_saslname(other.strip_prefix("a=")?)?),
    };
    if !parts.next()?.is_empty() {
        return None;
    }
    let mut token = None;
    for pair in rest.split('\x01') {
        if let Some(value) = pair.strip_prefix("auth=") {
            token = bearer(value);
        }
    }
    Some(SaslBearer { user, token: token? })
}

/// RFC 5801: `=2C` is a comma and `=3D` an equals sign in a SASL name.
fn decode_saslname(name: &str) -> Option<String> {
    let decoded = name.replace("=2C", ",").replace("=3D", "=");
    (!decoded.is_empty()).then_some(decoded)
}

/// XOAUTH2's client response: `user=` login `^A` `auth=Bearer token` `^A^A`.
pub fn parse_xoauth2(message: &[u8]) -> Option<SaslBearer> {
    let message = std::str::from_utf8(message).ok()?;
    let mut user = None;
    let mut token = None;
    for pair in message.split('\x01') {
        if let Some(value) = pair.strip_prefix("user=") {
            user = Some(value.trim().to_owned()).filter(|user| !user.is_empty());
        } else if let Some(value) = pair.strip_prefix("auth=") {
            token = bearer(value);
        }
    }
    Some(SaslBearer { user: Some(user?), token: token? })
}

/// Whether the login an app named fits the account its token belongs to. No login is fine.
pub fn sasl_user_matches(user: Option<&str>, login: &str) -> bool {
    user.is_none_or(|user| user.trim().eq_ignore_ascii_case(login))
}

/// The JSON a failed OAUTHBEARER or XOAUTH2 login is answered with before the final "no" (RFC 7628
/// section 3.2.2), saying where to get a new token.
pub fn sasl_bearer_error(xoauth2: bool, scope: &str, hostname: Option<&str>) -> String {
    let mut error = serde_json::json!({
        "status": if xoauth2 { "401" } else { "invalid_token" },
        "scope": scope,
    });
    if xoauth2 {
        error["schemes"] = "bearer".into();
    }
    if let Some(hostname) = hostname {
        error["openid-configuration"] = format!("https://{hostname}/.well-known/openid-configuration").into();
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_leaves_the_token_out() {
        let bearer = SaslBearer { user: Some("leni@example.org".into()), token: "secret-access-token".into() };
        let shown = format!("{bearer:?}");
        assert!(shown.contains("leni@example.org"), "{shown}");
        assert!(!shown.contains("secret-access-token"), "{shown}");
    }

    #[test]
    fn reads_what_apps_send() {
        // RFC 7628 section 4.1.
        let message = b"n,a=user@example.com,\x01host=server.example.com\x01port=143\x01auth=Bearer vF9dft4qmTc2Nvb3RlckBhbHRhdmlzdGEuY29tCg==\x01\x01";
        let parsed = parse_oauthbearer(message).unwrap();
        assert_eq!(parsed.user.as_deref(), Some("user@example.com"));
        assert_eq!(parsed.token, "vF9dft4qmTc2Nvb3RlckBhbHRhdmlzdGEuY29tCg==");
        let anonymous = parse_oauthbearer(b"n,,\x01auth=Bearer abc\x01\x01").unwrap();
        assert_eq!(anonymous, SaslBearer { user: None, token: "abc".into() });
        assert!(parse_oauthbearer(b"p=tls-unique,,\x01auth=Bearer abc\x01\x01").is_none());
        assert!(parse_oauthbearer(b"n,,\x01auth=Basic abc\x01\x01").is_none());
        assert!(parse_oauthbearer(b"\x01").is_none(), "the dummy answer after an error is no login");

        let xoauth2 = parse_xoauth2(
            b"user=someuser@example.com\x01auth=Bearer ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg\x01\x01",
        );
        assert_eq!(xoauth2.unwrap().user.as_deref(), Some("someuser@example.com"));
        assert!(parse_xoauth2(b"auth=Bearer abc\x01\x01").is_none(), "XOAUTH2 always names the login");

        assert!(sasl_user_matches(Some("Leni@Example.org"), "leni@example.org"));
        assert!(!sasl_user_matches(Some("nyu@example.org"), "leni@example.org"));
        assert!(sasl_user_matches(None, "leni@example.org"));
        let error: serde_json::Value =
            serde_json::from_str(&sasl_bearer_error(false, "mail", Some("mail.example.org"))).unwrap();
        assert_eq!(error["status"], "invalid_token");
        assert_eq!(error["openid-configuration"], "https://mail.example.org/.well-known/openid-configuration");
    }
}
