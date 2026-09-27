-- Moving from another provider (docs/moving.md): a person's old mailbox, copied over IMAP with the
-- password of that mailbox, in the background and in portions, until they say they are done. Then
-- the row goes, and the password with it.
--
-- The password is sealed with the same key as the passwords of fetched mailboxes (0027). Where each
-- folder of the old mailbox got is kept in import_progress (0022), so a run that was cut short, or
-- "sync again" a week later, only fetches what is new.
CREATE TABLE migration_jobs (
    id               INTEGER PRIMARY KEY,
    account_id       INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The old address, as the person typed it; for showing.
    address          TEXT NOT NULL,
    host             TEXT NOT NULL,
    port             INTEGER NOT NULL DEFAULT 993,
    login            TEXT NOT NULL,
    password_sealed  BLOB NOT NULL,
    -- queued: waiting for its turn; running: being copied now; paused: stopped by something the
    -- person has to look at (error says what); done: everything there was is here.
    state            TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'paused', 'done')),
    -- A code the portal turns into a sentence (quotaExceeded, loginRefused, unreachable, ...), and
    -- what the other server said, for the curious.
    error            TEXT NOT NULL DEFAULT '',
    error_detail     TEXT NOT NULL DEFAULT '',
    -- How far the current round got: the first copy, or a later "sync again".
    folders_done     INTEGER NOT NULL DEFAULT 0,
    folders_total    INTEGER NOT NULL DEFAULT 0,
    messages_done    INTEGER NOT NULL DEFAULT 0,
    messages_total   INTEGER NOT NULL DEFAULT 0,
    -- Messages that were already here and were left out; part of messages_done.
    messages_skipped INTEGER NOT NULL DEFAULT 0,
    bytes_done       INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    started_at       INTEGER,
    finished_at      INTEGER,
    last_run_at      INTEGER,
    UNIQUE (account_id, address)
);
CREATE INDEX migration_jobs_queued ON migration_jobs (state, last_run_at);
