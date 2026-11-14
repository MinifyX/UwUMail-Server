-- The built-in calendars (docs/holidays.md): the public holidays of a country or region and the
-- ISO week numbers, made from the settings `holidays` and `weekNumbers`. At most one of each kind
-- per account; the server writes their events, nobody else. `first_year` is the first year they
-- were written for, so the hourly job knows when to write them for the next years.
CREATE TABLE builtin_calendars (
    collection_id INTEGER PRIMARY KEY REFERENCES dav_collections (id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('holidays', 'weeks')),
    region TEXT NOT NULL DEFAULT '',
    language TEXT NOT NULL,
    first_year INTEGER NOT NULL,
    UNIQUE (account_id, kind)
);
