-- The change log (JMAP */changes) and the vanished UIDs (IMAP QRESYNC) only keep 90 days. Neither
-- has a time, so once a day the account's modseq is noted with the time; changes up to the mark
-- that is old enough go, and `changes_floor` remembers the lowest state that can still be answered.
ALTER TABLE accounts ADD COLUMN changes_floor INTEGER NOT NULL DEFAULT 0;

CREATE TABLE change_marks (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    at         INTEGER NOT NULL,
    modseq     INTEGER NOT NULL,
    PRIMARY KEY (account_id, at)
) WITHOUT ROWID;
