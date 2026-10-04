-- Why messages of a move were left out, counted apart (messages_skipped stays the sum): the
-- folder's mailbox here held them already, they were larger than this server takes, or they could
-- not be read safely (nested too deep, too many parts). Rows from before count only in the sum.
ALTER TABLE move_mailboxes ADD COLUMN messages_known INTEGER NOT NULL DEFAULT 0;
ALTER TABLE move_mailboxes ADD COLUMN messages_too_large INTEGER NOT NULL DEFAULT 0;
ALTER TABLE move_mailboxes ADD COLUMN messages_unreadable INTEGER NOT NULL DEFAULT 0;
ALTER TABLE migration_jobs ADD COLUMN messages_known INTEGER NOT NULL DEFAULT 0;
ALTER TABLE migration_jobs ADD COLUMN messages_too_large INTEGER NOT NULL DEFAULT 0;
ALTER TABLE migration_jobs ADD COLUMN messages_unreadable INTEGER NOT NULL DEFAULT 0;

-- The messages a move left out, so the admin or the person can see which: the folder at the old
-- provider (its decoded path), the UID there, why, and what the headers said. At most a thousand per
-- mailbox or job (crate::move_skipped::MAX_SKIPPED_LISTED); the counts above go on counting.
-- reason: known, tooLarge, unreadable.
CREATE TABLE move_mailbox_skipped (
    move_mailbox_id INTEGER NOT NULL REFERENCES move_mailboxes (id) ON DELETE CASCADE,
    folder          TEXT NOT NULL,
    uid             INTEGER NOT NULL,
    reason          TEXT NOT NULL CHECK (reason IN ('known', 'tooLarge', 'unreadable')),
    sender          TEXT NOT NULL DEFAULT '',
    subject         TEXT NOT NULL DEFAULT '',
    sent_at         INTEGER,
    size            INTEGER NOT NULL DEFAULT 0,
    recorded_at     INTEGER NOT NULL,
    PRIMARY KEY (move_mailbox_id, folder, uid, reason)
);

CREATE TABLE migration_job_skipped (
    job_id      INTEGER NOT NULL REFERENCES migration_jobs (id) ON DELETE CASCADE,
    folder      TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    reason      TEXT NOT NULL CHECK (reason IN ('known', 'tooLarge', 'unreadable')),
    sender      TEXT NOT NULL DEFAULT '',
    subject     TEXT NOT NULL DEFAULT '',
    sent_at     INTEGER,
    size        INTEGER NOT NULL DEFAULT 0,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (job_id, folder, uid, reason)
);
