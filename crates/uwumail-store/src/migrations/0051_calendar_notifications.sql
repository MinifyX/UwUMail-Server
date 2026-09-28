-- What others did to one's events: the CalendarEventNotifications of JMAP Calendars
-- (docs/jmap-calendars.md). A change is kept once, with the event before and after it as
-- iCalendar; everyone it is told to has a notification pointing to it until they dismiss it.
CREATE TABLE calendar_changes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The event; it may be gone since.
    resource_id INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    -- Who did it: a person of this server, or someone elsewhere (NULL) by name and address.
    by_account  INTEGER,
    by_name     TEXT NOT NULL,
    by_email    TEXT,
    by_address  TEXT,
    comment     TEXT,
    is_draft    INTEGER NOT NULL DEFAULT 0,
    old_content TEXT,
    new_content TEXT
);

CREATE TABLE calendar_notifications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    change_id  INTEGER NOT NULL REFERENCES calendar_changes (id) ON DELETE CASCADE,
    type       TEXT NOT NULL CHECK (type IN ('created', 'updated', 'destroyed')),
    event_id   INTEGER NOT NULL
);
CREATE INDEX calendar_notifications_account ON calendar_notifications (account_id, id);
CREATE INDEX calendar_notifications_change ON calendar_notifications (change_id);
