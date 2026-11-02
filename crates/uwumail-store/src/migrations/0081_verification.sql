-- One-time codes and sign-in links found in delivered mail (docs/verification-codes.md), one row per
-- stored email. `source` is `header` (a signed Verification-Info header) or `heuristic` (guessed
-- from the text; only kept when DMARC passed). `author_domain` is the registrable From domain, for
-- the limit of codes per sender and hour. `cleaned`: the cleanup has dealt with the mail (moved it
-- to the Trash, left it because it was flagged, no longer in the Inbox or the cleanup is off), so it
-- never looks again. Rows go after a retention of 30 days (security-audit-0.26.0 VC-06).
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
-- What the cleanup still has to look at; rows it dealt with drop out of the index.
CREATE INDEX email_verification_due ON email_verification(account_id, expires_at) WHERE cleaned = 0;
-- VerificationCode/query, newest first, paged in SQL.
CREATE INDEX email_verification_recent ON email_verification(account_id, created, email_id);
CREATE INDEX email_verification_sender ON email_verification(account_id, author_domain, created);
-- The retention.
CREATE INDEX email_verification_created ON email_verification(created);

-- Every time a signed code let a mail skip greylisting, counted when it was decided, whatever then
-- became of the mail (Junk, discarded, forwarded): the limits per sender and per account hold for
-- the fast lane itself (VC-02). Rows older than an hour are of no use and go.
CREATE TABLE verification_fast_lane (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    author_domain TEXT NOT NULL,
    created INTEGER NOT NULL
);
CREATE INDEX verification_fast_lane_sender ON verification_fast_lane(account_id, author_domain, created);
CREATE INDEX verification_fast_lane_account ON verification_fast_lane(account_id, created);
CREATE INDEX verification_fast_lane_created ON verification_fast_lane(created);
