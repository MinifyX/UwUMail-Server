-- Calendars and address books for CalDAV and CardDAV.
CREATE TABLE dav_collections (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    -- The last segment of the collection's URL.
    slug         TEXT NOT NULL,
    display_name TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    -- Apple's calendar-color, like #FF4D8DFF.
    color        TEXT,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    -- Calendar component types, like "VEVENT VTODO".
    components   TEXT NOT NULL DEFAULT '',
    -- A VCALENDAR with the calendar's time zone, when a client set one.
    timezone     TEXT,
    -- Counts every change of the collection's entries; sync tokens and CTags are made from it.
    change       INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    UNIQUE (account_id, kind, slug)
);

-- Events, tasks and contacts: one iCalendar or vCard object per resource.
CREATE TABLE dav_resources (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    -- The last segment of the resource's URL, like "c0ffee.ics".
    name          TEXT NOT NULL,
    uid           TEXT NOT NULL,
    etag          TEXT NOT NULL,
    content       TEXT NOT NULL,
    -- VEVENT, VTODO, VJOURNAL or VCARD.
    component     TEXT NOT NULL DEFAULT '',
    -- The time the object covers, for calendar queries; NULL when unknown or open-ended.
    starts_at     INTEGER,
    ends_at       INTEGER,
    size          INTEGER NOT NULL,
    modified_at   INTEGER NOT NULL,
    change        INTEGER NOT NULL,
    UNIQUE (collection_id, name)
);
CREATE INDEX dav_resources_uid ON dav_resources (collection_id, uid);
CREATE INDEX dav_resources_change ON dav_resources (collection_id, change);

-- Resources that were deleted, so sync-collection can report them.
CREATE TABLE dav_tombstones (
    collection_id INTEGER NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    change        INTEGER NOT NULL,
    PRIMARY KEY (collection_id, name)
) WITHOUT ROWID;
