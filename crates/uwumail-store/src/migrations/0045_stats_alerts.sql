-- Numbers for the admin panel's statistics (docs/admin-alerts.md): the server counts what happens in
-- memory (mail received, refused, sent, failed logins, …) and adds it to the day's row every minute.
-- `day` is the UTC date (`2026-09-27`), `key` a stable name such as `mail.received` or
-- `refused.spam`. Keys starting with `gauge.` hold the day's last reading instead of a sum.
CREATE TABLE stats_daily (
    day   TEXT NOT NULL,
    key   TEXT NOT NULL,
    value INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, key)
) WITHOUT ROWID;

-- What the admins were told about: a health finding that turned yellow or red, a failed or old
-- backup, a certificate that cannot be renewed, a new version (information only). An alert is open
-- while `resolved_at` is NULL; there is at most one open alert per kind and key, and resolved ones
-- stay as history for a while.
CREATE TABLE alerts (
    id              INTEGER PRIMARY KEY,
    -- The health area (`dns`, `delivery`, …) or `backup`, `certificate`, `update`.
    kind            TEXT NOT NULL,
    -- What exactly, stable while it lasts: the finding's code, and its domain when it has one.
    key             TEXT NOT NULL,
    -- The finding code the portal turns into a sentence.
    code            TEXT NOT NULL,
    level           TEXT NOT NULL CHECK (level IN ('info', 'warning', 'problem')),
    params          TEXT NOT NULL DEFAULT '{}',
    -- The portal page where it can be fixed.
    link            TEXT,
    first_seen      INTEGER NOT NULL,
    last_seen       INTEGER NOT NULL,
    resolved_at     INTEGER,
    -- When the admins were last written to about it, and at which level.
    notified_at     INTEGER,
    notified_level  TEXT,
    -- An admin said they know; no more reminders then.
    acknowledged_at INTEGER,
    acknowledged_by TEXT
);
CREATE UNIQUE INDEX alerts_open ON alerts (kind, key) WHERE resolved_at IS NULL;
CREATE INDEX alerts_resolved ON alerts (resolved_at);
