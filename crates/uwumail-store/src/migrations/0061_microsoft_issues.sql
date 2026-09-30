-- Refusals and throttling by Microsoft's mail servers (docs/microsoft.md): one open issue per
-- sending address or sender domain and code, with when it was first and last seen and how often.
-- `scope` is 'ip' or 'domain', `subject` the address or the domain ('' when it is not known).
-- `grp` is what the answer means (blockList, banned, ipRefused, throttled, authentication, dmarc).
CREATE TABLE microsoft_issues (
    id          INTEGER PRIMARY KEY,
    scope       TEXT NOT NULL CHECK (scope IN ('ip', 'domain')),
    subject     TEXT NOT NULL,
    grp         TEXT NOT NULL,
    code        TEXT NOT NULL,
    ip          TEXT NOT NULL DEFAULT '',
    domain      TEXT NOT NULL DEFAULT '',
    reply       TEXT NOT NULL,
    first_seen  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL,
    count       INTEGER NOT NULL DEFAULT 1,
    resolved_at INTEGER,
    resolved_by TEXT
);
CREATE UNIQUE INDEX microsoft_issues_open ON microsoft_issues (scope, subject, code) WHERE resolved_at IS NULL;

-- The last mail Microsoft accepted, per sending address and sender domain: an issue is over once
-- mail went through after it and no new refusal came for a day.
CREATE TABLE microsoft_deliveries (
    scope   TEXT NOT NULL,
    subject TEXT NOT NULL,
    at      INTEGER NOT NULL,
    PRIMARY KEY (scope, subject)
) WITHOUT ROWID;
