-- Moves the admin runs (docs/moving.md, "For admins"): a whole domain with all its people, or one
-- mailbox, copied from another server over IMAP, with contacts and calendars over CardDAV/CalDAV.
-- Unlike a person's own move (migration_jobs, 0040) it keeps copying what arrives at the old
-- server until the admin says it is over ("finish"), after the MX records point here. Then a last
-- round runs and the passwords of the old mailboxes are wiped.
CREATE TABLE moves (
    id                  INTEGER PRIMARY KEY,
    -- domain: everyone of a domain; mailbox: one mailbox, new or filled.
    kind                TEXT NOT NULL CHECK (kind IN ('domain', 'mailbox')),
    -- The domain here the mailboxes are on.
    domain              TEXT NOT NULL,
    -- The old server everyone of this move shares; a mailbox may name its own.
    imap_host           TEXT NOT NULL,
    imap_port           INTEGER NOT NULL DEFAULT 993,
    -- How contacts and calendars are found: auto, nextcloud, sogo, icloud, gmx, webde, custom or
    -- none; dav_host is the server for nextcloud and sogo (the IMAP host when empty), dav_url the
    -- address for custom.
    dav_mode            TEXT NOT NULL DEFAULT 'auto',
    dav_host            TEXT NOT NULL DEFAULT '',
    dav_url             TEXT NOT NULL DEFAULT '',
    contacts            INTEGER NOT NULL DEFAULT 1,
    calendars           INTEGER NOT NULL DEFAULT 1,
    -- How many mailboxes are copied at once, so the old server is not overrun.
    parallel            INTEGER NOT NULL DEFAULT 2,
    -- How long a mailbox that is up to date waits before its next round.
    sync_minutes        INTEGER NOT NULL DEFAULT 60,
    -- active: copying and keeping up; paused: by the admin; finishing: the last round runs;
    -- done: over, every password wiped.
    state               TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'paused', 'finishing', 'done')),
    created_by          INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    created_at          INTEGER NOT NULL,
    finish_requested_at INTEGER,
    finished_at         INTEGER
);

CREATE TABLE move_mailboxes (
    id               INTEGER PRIMARY KEY,
    move_id          INTEGER NOT NULL REFERENCES moves (id) ON DELETE CASCADE,
    account_id       INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The address at the old server, for showing.
    old_address      TEXT NOT NULL,
    login            TEXT NOT NULL,
    -- Sealed like the passwords of fetched mailboxes (0027); NULL once the move is finished.
    password_sealed  BLOB,
    -- Its own old server, when it is not the move's; empty and 0 otherwise.
    imap_host        TEXT NOT NULL DEFAULT '',
    imap_port        INTEGER NOT NULL DEFAULT 0,
    -- Its own CalDAV/CardDAV address, when autodiscovery does not find it.
    dav_url          TEXT NOT NULL DEFAULT '',
    -- 1 when this move created the mailbox (it gets an invitation link), 0 when it filled one
    -- that was there (its person keeps their password).
    created_account  INTEGER NOT NULL DEFAULT 0,
    -- queued: waiting for its turn; running: being copied now; paused: stopped, error says why;
    -- synced: up to date, the next round comes at next_sync_at; done: finished, password wiped.
    state            TEXT NOT NULL DEFAULT 'queued'
                     CHECK (state IN ('queued', 'running', 'paused', 'synced', 'done')),
    -- Set when the admin finished the move: the next round that ends is the last.
    final_round      INTEGER NOT NULL DEFAULT 0,
    error            TEXT NOT NULL DEFAULT '',
    error_detail     TEXT NOT NULL DEFAULT '',
    -- Counted over all rounds.
    folders_done     INTEGER NOT NULL DEFAULT 0,
    folders_total    INTEGER NOT NULL DEFAULT 0,
    messages_done    INTEGER NOT NULL DEFAULT 0,
    messages_total   INTEGER NOT NULL DEFAULT 0,
    messages_skipped INTEGER NOT NULL DEFAULT 0,
    bytes_done       INTEGER NOT NULL DEFAULT 0,
    -- What the old mailbox holds altogether, when its server says (for the quota warning).
    source_bytes     INTEGER,
    contacts_done    INTEGER NOT NULL DEFAULT 0,
    events_done      INTEGER NOT NULL DEFAULT 0,
    -- Why contacts or calendars did not come (a code), empty when they did or were not asked for.
    dav_error        TEXT NOT NULL DEFAULT '',
    -- The calendars and address books found in the first round, as JSON, so later rounds ask the
    -- same addresses even when the domain's DNS points here by then.
    dav_found        TEXT NOT NULL DEFAULT '',
    rounds           INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    last_run_at      INTEGER,
    last_synced_at   INTEGER,
    next_sync_at     INTEGER,
    finished_at      INTEGER,
    UNIQUE (move_id, account_id)
);
CREATE INDEX move_mailboxes_queue ON move_mailboxes (state, last_run_at);
CREATE INDEX move_mailboxes_account ON move_mailboxes (account_id);
