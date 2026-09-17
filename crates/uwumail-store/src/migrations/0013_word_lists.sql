-- Words, phrases and Rspamd-style regular expressions that count against a message when they show up in its
-- subject or text. Like sender lists, an entry belongs to the whole server (no account and no domain), one of
-- our domains or one person. Entries are added by hand or come from a subscribed list.

-- Lists subscribed by link and fetched again every day.
CREATE TABLE word_sources (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    domain_id    INTEGER REFERENCES domains (id) ON DELETE CASCADE,
    url          TEXT NOT NULL,
    subject_only INTEGER NOT NULL DEFAULT 0,
    points       REAL,              -- for each entry; NULL means the default
    fetched_at   INTEGER,           -- last attempt
    validator    TEXT,              -- ETag or Last-Modified of the last good fetch
    error        TEXT,              -- why the last attempt failed; NULL after a good one
    created_at   INTEGER NOT NULL,
    created_by   TEXT NOT NULL DEFAULT '',
    CHECK (account_id IS NULL OR domain_id IS NULL)
);
CREATE UNIQUE INDEX word_sources_url ON word_sources (COALESCE(account_id, 0), COALESCE(domain_id, 0), url);

CREATE TABLE word_entries (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    domain_id  INTEGER REFERENCES domains (id) ON DELETE CASCADE,
    source_id  INTEGER REFERENCES word_sources (id) ON DELETE CASCADE,
    pattern    TEXT NOT NULL,       -- a word or phrase in lower case, or /regex/flags as written
    points     REAL,                -- NULL means the source's or the default
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    CHECK (account_id IS NULL OR domain_id IS NULL)
);
CREATE UNIQUE INDEX word_entries_pattern
    ON word_entries (COALESCE(account_id, 0), COALESCE(domain_id, 0), COALESCE(source_id, 0), pattern);
CREATE INDEX word_entries_source ON word_entries (source_id);

-- Bumped with every change to lists the SMTP side compiles, so it knows when to compile them again.
CREATE TABLE list_versions (
    name    TEXT PRIMARY KEY,
    version INTEGER NOT NULL
);
