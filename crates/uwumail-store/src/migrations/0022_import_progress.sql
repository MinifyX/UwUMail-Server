-- How far copying mail from another server got, per person and folder, so the next run only fetches
-- what arrived since. A changed UIDVALIDITY means the old server renumbered the folder.
CREATE TABLE import_progress (
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    source       TEXT NOT NULL,     -- e.g. "imap.example.com"
    folder       TEXT NOT NULL,     -- the folder name on the old server
    uid_validity INTEGER NOT NULL,
    last_uid     INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (account_id, source, folder)
);
