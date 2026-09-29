-- The birthdays calendar (docs/birthdays.md): one per account, filled from the birthdays and
-- anniversaries in the account's own address books and written by nothing else. `language` is the
-- one its titles are in ('de' or 'en'); when the person picks another, the calendar is written anew.
CREATE TABLE birthday_calendars (
    account_id    INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL UNIQUE REFERENCES dav_collections (id) ON DELETE CASCADE,
    language      TEXT NOT NULL
);

-- People who have birthdays in their cards already get the calendar once the server runs again.
INSERT INTO settings (key, value) VALUES ('birthdays.backfill', 'pending')
    ON CONFLICT (key) DO UPDATE SET value = excluded.value;
