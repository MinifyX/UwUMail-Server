-- Shared calendars and address books, and calendar scheduling (docs/calendars.md).
--
-- A calendar or address book its owner shares with another person of this server. `rights` is
-- what that person may do with it: `read` its entries, `write` them too, or `all`, which also lets
-- them rename it, change its colour and share it with others. Deleting it stays its owner's.
CREATE TABLE dav_shares (
    collection_id INTEGER NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    rights        TEXT NOT NULL CHECK (rights IN ('read', 'write', 'all')),
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (collection_id, account_id)
) WITHOUT ROWID;
CREATE INDEX dav_shares_account ON dav_shares (account_id);

-- The Schedule-Tag of CalDAV scheduling (RFC 6638): like the ETag, except that it stays when the
-- server only writes an attendee's answer into the organizer's copy, so the organizer's client can
-- still store its own change on top.
ALTER TABLE dav_resources ADD COLUMN schedule_tag TEXT;
UPDATE dav_resources SET schedule_tag = etag;
