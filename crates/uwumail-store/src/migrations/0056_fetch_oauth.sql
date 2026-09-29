-- Fetched mailboxes that log in with OAuth instead of a password: Microsoft (Outlook.com, Hotmail,
-- Microsoft 365) and Google. Microsoft has switched plain passwords off for IMAP and SMTP at most
-- mailboxes, so for them this is the only way in left.
--
-- The refresh token opens the mailbox for as long as the grant lasts, so it is sealed like the
-- password (see fetch.rs); so is the short-lived access token made from it. The password column
-- stays, holding a sealed empty password for these rows.
ALTER TABLE fetch_accounts ADD COLUMN auth TEXT NOT NULL DEFAULT 'password'
    CHECK (auth IN ('password', 'microsoft', 'google'));
ALTER TABLE fetch_accounts ADD COLUMN oauth_refresh BLOB;
ALTER TABLE fetch_accounts ADD COLUMN oauth_access BLOB;
-- When the access token stops working; it is renewed a little before.
ALTER TABLE fetch_accounts ADD COLUMN oauth_expires_at INTEGER;
-- The provider ended the grant (revoked, expired, password changed): nothing is asked of it any
-- more until the person signs in again.
ALTER TABLE fetch_accounts ADD COLUMN oauth_expired INTEGER NOT NULL DEFAULT 0;
-- After a token endpoint that did not answer, it is not asked again before this; the wait grows
-- with every failure in a row, so a provider having a bad day is not hammered.
ALTER TABLE fetch_accounts ADD COLUMN oauth_retry_at INTEGER;
ALTER TABLE fetch_accounts ADD COLUMN oauth_failures INTEGER NOT NULL DEFAULT 0;
-- The provider said it takes no passwords at all any more (Microsoft: "Basic authentication is
-- disabled"). Not a wrong password: runs stop asking until the mailbox switches to signing in, or
-- somebody asks for a run by hand, and the person is told once.
ALTER TABLE fetch_accounts ADD COLUMN password_refused INTEGER NOT NULL DEFAULT 0;
