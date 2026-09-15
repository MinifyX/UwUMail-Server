-- Logins to the web portal. The cookie holds a random token; only its SHA-256 is stored.
CREATE TABLE web_sessions (
    token_hash   BLOB PRIMARY KEY,
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    csrf_token   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    ip           TEXT NOT NULL DEFAULT '',
    user_agent   TEXT NOT NULL DEFAULT ''
) WITHOUT ROWID;
CREATE INDEX web_sessions_account ON web_sessions (account_id);
CREATE INDEX web_sessions_expires ON web_sessions (expires_at);

-- Personal settings of the web portal (language, tone, Simple/Pro, theme) as a JSON object.
ALTER TABLE accounts ADD COLUMN preferences TEXT NOT NULL DEFAULT '{}';
