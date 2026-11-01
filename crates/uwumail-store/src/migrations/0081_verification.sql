-- One-time codes and sign-in links found in delivered mail (docs/verification-codes.md), one row per
-- stored email. `source` is `header` (a signed Verification-Info header) or `heuristic` (guessed
-- from the text). `author_domain` is the registrable From domain, for the limit of codes per sender
-- and hour. `cleaned`: the cleanup has dealt with the mail (moved it to the Trash, or left it because
-- it was flagged or no longer in the Inbox), so it never looks again.
CREATE TABLE email_verification (
    email_id INTEGER PRIMARY KEY REFERENCES emails(id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    origin TEXT NOT NULL,
    code TEXT,
    url TEXT,
    purpose TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    autofill INTEGER NOT NULL,
    author_domain TEXT NOT NULL,
    created INTEGER NOT NULL,
    cleaned INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX email_verification_expiry ON email_verification(account_id, expires_at);
CREATE INDEX email_verification_sender ON email_verification(account_id, author_domain, created);
