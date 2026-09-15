-- Second factors, app passwords and what happened to each person's security.

-- Mail apps may only log in with app passwords, even without a second factor.
ALTER TABLE accounts ADD COLUMN apps_need_app_password INTEGER NOT NULL DEFAULT 0;
-- Bumped when a password, app password or second factor changes, so cached logins end.
ALTER TABLE accounts ADD COLUMN credentials_changed_at INTEGER NOT NULL DEFAULT 0;

-- The shared secret of an authenticator app. Unconfirmed while being set up.
CREATE TABLE totp_secrets (
    account_id   INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    secret       BLOB NOT NULL,
    created_at   INTEGER NOT NULL,
    confirmed_at INTEGER,
    last_step    INTEGER NOT NULL DEFAULT 0 -- a code works only once
);

-- One-time codes for when the second factor is lost. Only their SHA-256 is stored.
CREATE TABLE recovery_codes (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    code_hash  BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    used_at    INTEGER
);
CREATE INDEX recovery_codes_account ON recovery_codes (account_id);

-- WebAuthn credentials (passkeys, security keys) used as a second factor.
CREATE TABLE passkeys (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    credential_id BLOB NOT NULL UNIQUE,
    public_key    BLOB NOT NULL, -- COSE key as sent by the authenticator
    sign_count    INTEGER NOT NULL DEFAULT 0,
    name          TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);
CREATE INDEX passkeys_account ON passkeys (account_id);

-- Passwords for mail apps. They are random, so a SHA-256 is enough and makes lookups cheap.
CREATE TABLE app_passwords (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    secret_hash        BLOB NOT NULL UNIQUE,
    scopes             TEXT NOT NULL,          -- space-separated: "mail", "smtp"
    created_at         INTEGER NOT NULL,
    expires_at         INTEGER,
    last_used_at       INTEGER,
    last_used_protocol TEXT,
    last_used_ip       TEXT
);
CREATE INDEX app_passwords_account ON app_passwords (account_id);

-- "Recent activity" in My account: logins and changes to a person's security.
CREATE TABLE security_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    at         INTEGER NOT NULL,
    kind       TEXT NOT NULL,              -- e.g. "passwordChanged"
    actor      TEXT NOT NULL DEFAULT '',   -- empty for the person, otherwise an admin's login or "cli"
    ip         TEXT NOT NULL DEFAULT '',
    details    TEXT NOT NULL DEFAULT '{}'  -- JSON, never secrets
);
CREATE INDEX security_events_account ON security_events (account_id, at);
