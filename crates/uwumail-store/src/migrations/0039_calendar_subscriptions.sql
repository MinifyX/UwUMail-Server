-- Calendars subscribed to by address (docs/calendar-import.md): a feed in iCalendar format, such as
-- the secret address of a Google calendar, a holiday calendar or a club's fixtures, fetched again
-- every so often. The calendar it fills is a mirror: nobody writes into it but the feed, so every
-- CalDAV and JMAP write into it is refused, and it is left out of free-busy and scheduling.
--
-- The address is often a credential by itself (Google's secret address is), so it is sealed with
-- the same key as the passwords of fetched mailboxes (0027). `url_shown` is what the portal and
-- the log show instead: the host and no path.
CREATE TABLE calendar_subscriptions (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL UNIQUE REFERENCES dav_collections (id) ON DELETE CASCADE,
    url           BLOB NOT NULL,
    -- SHA-256 of the address, to notice the same feed subscribed twice without unsealing.
    url_digest    TEXT NOT NULL,
    url_shown     TEXT NOT NULL,
    interval_secs INTEGER NOT NULL DEFAULT 3600 CHECK (interval_secs BETWEEN 900 AND 604800),
    -- Reminders of a feed ring on every device; they are dropped unless asked for.
    keep_alarms   INTEGER NOT NULL DEFAULT 0,
    enabled       INTEGER NOT NULL DEFAULT 1,
    -- What the feed answered last time (`etag:"…"` or `modified:…`), to ask only for news.
    validator     TEXT,
    next_run_at   INTEGER NOT NULL DEFAULT 0,
    last_run_at   INTEGER,
    last_ok_at    INTEGER,
    last_error    TEXT NOT NULL DEFAULT '',
    -- Failed runs in a row; the next try waits longer after each.
    failures      INTEGER NOT NULL DEFAULT 0,
    entries       INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    UNIQUE (account_id, url_digest)
);
CREATE INDEX calendar_subscriptions_due ON calendar_subscriptions (enabled, next_run_at);
