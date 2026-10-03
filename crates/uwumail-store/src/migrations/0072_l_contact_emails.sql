-- Which card names which address, for every card (not only those with a photo, see 0055
-- contact_photos), so "does the reader know this sender" is one index lookup at delivery instead of
-- reading and lower-casing every vCard of the account, photos included (security review 0.22
-- LABELS22-M2). Exact addresses, lower case: a prefix of a contact's address is not the contact
-- (LABELS22-L1). Kept with every write of a card over CardDAV and JMAP; a card that goes takes its
-- rows with it. `account_id` is the address book's owner. Filled for the cards already there when
-- the server starts after this migration.
CREATE TABLE contact_emails (
    resource_id   INTEGER NOT NULL REFERENCES dav_resources (id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL,
    account_id    INTEGER NOT NULL,
    email         TEXT NOT NULL,
    PRIMARY KEY (resource_id, email)
) WITHOUT ROWID;
CREATE INDEX contact_emails_account ON contact_emails (account_id, email);
INSERT INTO settings (key, value) VALUES ('contact_emails.backfill', 'pending')
    ON CONFLICT (key) DO UPDATE SET value = excluded.value;
