-- What each person keeps for themselves about a calendar and its events (docs/jmap-calendars.md).
--
-- Someone a calendar is shared with gives it their own name, colour, order, visibility and time
-- zone without touching the owner's; for the owner these stay in `dav_collections`, where CalDAV
-- keeps them. Whether a calendar makes its person busy and the alerts new events get by default
-- are kept here for everyone, owner included. NULL means "not set": the owner's value, or the
-- default.
CREATE TABLE calendar_prefs (
    collection_id               INTEGER NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    account_id                  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name                        TEXT,
    color                       TEXT,
    sort_order                  INTEGER,
    is_visible                  INTEGER,
    timezone                    TEXT,
    include_in_availability     TEXT CHECK (include_in_availability IN ('all', 'attending', 'none')),
    -- JSCalendar Alert maps as JSON.
    default_alerts_with_time    TEXT,
    default_alerts_without_time TEXT,
    PRIMARY KEY (collection_id, account_id)
) WITHOUT ROWID;
CREATE INDEX calendar_prefs_account ON calendar_prefs (account_id);

-- The per-user properties of an event (keywords, color, freeBusyStatus, useDefaultAlerts, alerts,
-- also per instance) for someone the calendar is shared with, as JSON. The owner's are part of
-- the event itself.
CREATE TABLE calendar_event_prefs (
    resource_id INTEGER NOT NULL REFERENCES dav_resources (id) ON DELETE CASCADE,
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    data        TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (resource_id, account_id)
) WITHOUT ROWID;
CREATE INDEX calendar_event_prefs_account ON calendar_event_prefs (account_id);
