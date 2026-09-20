-- Fetch accounts: mailboxes at other providers that this server empties into someone's mailbox.
-- A free mail address one is forced to hand out, an old address from years ago -- the mail should
-- arrive where all the other mail arrives, in every app, and go through this server's spam filter
-- like anything else.
--
-- The password is the provider's, not ours, so it cannot be hashed: we have to send it. It is
-- sealed with a key of this server (see fetch.rs). That key lives in the same database, so this
-- protects an extract that leaves the house, not someone who holds the whole database.
CREATE TABLE fetch_accounts (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The address at the provider, normalized. Only for showing and for recognizing mail meant for it.
    address       TEXT NOT NULL,
    host          TEXT NOT NULL,
    port          INTEGER NOT NULL DEFAULT 993,
    security      TEXT NOT NULL DEFAULT 'tls' CHECK (security IN ('tls', 'starttls')),
    username      TEXT NOT NULL,
    password      BLOB NOT NULL,
    -- What happens to a message at the provider once this server has taken it.
    after_fetch   TEXT NOT NULL DEFAULT 'mark_read' CHECK (after_fetch IN ('mark_read', 'delete')),
    -- Whether the provider's own junk folder is emptied too. It is, by default: this server judges
    -- that mail again itself, and the provider's verdict only adds points.
    fetch_junk    INTEGER NOT NULL DEFAULT 1,
    interval_secs INTEGER NOT NULL DEFAULT 300,
    enabled       INTEGER NOT NULL DEFAULT 1,
    -- The name the provider writes its Authentication-Results under. Empty means: whatever name
    -- belongs to the provider's own domain. Only the topmost header with that name is believed.
    auth_serv_id  TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    -- How the last run went, for the portal.
    last_run_at   INTEGER,
    last_ok_at    INTEGER,
    last_error    TEXT NOT NULL DEFAULT '',
    last_fetched  INTEGER NOT NULL DEFAULT 0,
    total_fetched INTEGER NOT NULL DEFAULT 0,
    UNIQUE (account_id, address)
);

-- Where each folder of a fetch account stands, so a run only takes what arrived since.
--
-- A message the server did not take right away -- greylisting asks every sender to come back later,
-- and a full mailbox says so too -- stays at the provider and stops the folder at its UID: the next
-- run offers it again, which is exactly what greylisting asks of a sending server. held_since says
-- since when, so a message that never gets through can be stepped over instead of blocking a folder
-- forever.
CREATE TABLE fetch_state (
    fetch_id     INTEGER NOT NULL REFERENCES fetch_accounts (id) ON DELETE CASCADE,
    folder       TEXT NOT NULL,
    uid_validity INTEGER NOT NULL,
    last_uid     INTEGER NOT NULL DEFAULT 0,
    held_uid     INTEGER,
    held_since   INTEGER,
    PRIMARY KEY (fetch_id, folder)
);

-- Messages this fetch account already brought, so none of them arrives twice: a provider that
-- renumbers its folder (a new UIDVALIDITY) starts the count over, and then only the message itself
-- says whether it was here before. Kept for a while and then forgotten, see fetch.rs.
CREATE TABLE fetch_seen (
    fetch_id INTEGER NOT NULL REFERENCES fetch_accounts (id) ON DELETE CASCADE,
    -- The message's own name where it has one, otherwise the hash of its bytes.
    key      TEXT NOT NULL,
    seen_at  INTEGER NOT NULL,
    PRIMARY KEY (fetch_id, key)
);

CREATE INDEX fetch_seen_age ON fetch_seen (seen_at);
