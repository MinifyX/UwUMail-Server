-- Earlier versions of recurring events (docs/jmap-calendars.md), so CalendarEvent/queryChanges of
-- a query that expands recurrences knows which instances the query had before a change. For
-- every change of an event that recurs before or after it, each account that sees the event keeps
-- a row: the account's modseq of that change and the event before it (NULL when it did not recur
-- then). Versions are kept for 30 days; `calendar_event_versions_since` says from which state on
-- an account's are complete.
CREATE TABLE calendar_event_contents (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    hash    TEXT NOT NULL UNIQUE,
    content TEXT NOT NULL
);

CREATE TABLE calendar_event_versions (
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    resource_id INTEGER NOT NULL,
    modseq      INTEGER NOT NULL,
    content_id  INTEGER REFERENCES calendar_event_contents (id),
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (account_id, resource_id, modseq)
) WITHOUT ROWID;
CREATE INDEX calendar_event_versions_content ON calendar_event_versions (content_id);
CREATE INDEX calendar_event_versions_age ON calendar_event_versions (account_id, created_at);

CREATE TABLE calendar_event_versions_since (
    account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    modseq     INTEGER NOT NULL
);
INSERT INTO calendar_event_versions_since (account_id, modseq) SELECT id, modseq FROM accounts;
