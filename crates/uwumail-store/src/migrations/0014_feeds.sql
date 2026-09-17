-- Built-in lists the server fetches itself: malware links and attachment hashes, throwaway and freemail
-- domains, link shorteners and spam subjects. The settings switch them on and off; this keeps what was fetched.
CREATE TABLE feed_state (
    key        TEXT PRIMARY KEY,
    fetched_at INTEGER,            -- last attempt
    changed_at INTEGER,            -- last fetch that brought a new list
    validator  TEXT,               -- ETag or Last-Modified of that fetch
    error      TEXT                -- why the last attempt failed; NULL after a good one
);

CREATE TABLE feed_entries (
    key   TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (key, value)
) WITHOUT ROWID;
