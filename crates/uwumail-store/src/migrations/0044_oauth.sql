-- Signing in without app passwords (docs/oauth.md, docs/login-oidc-ldap.md).
--
-- The server is an OAuth 2.0 / OpenID Connect provider for mail apps: an app registers itself
-- (RFC 7591), the person agrees once in the portal, and the app gets short-lived access tokens and
-- a refresh token it trades in for new ones. Every token and code is kept only as a SHA-256 hash,
-- like app passwords: they are long random strings, so a plain hash is enough.
--
-- A client is an app as it registered itself. Every one is a public client without a secret: mail
-- apps run on the person's own device and could not keep one, so PKCE ties a code to the app
-- that asked for it.
CREATE TABLE oauth_clients (
    id            INTEGER PRIMARY KEY,
    client_id     TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    -- One redirect address per line, compared exactly (loopback addresses may change the port,
    -- RFC 8252 section 7.3).
    redirect_uris TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);

-- A grant is one sign-in of one app on one device: what the person allowed it, and when it was
-- last used. Revoking it ends every token that came from it.
CREATE TABLE oauth_grants (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    client_id          INTEGER NOT NULL REFERENCES oauth_clients (id) ON DELETE CASCADE,
    -- Space-separated, as OAuth writes them: mail smtp dav openid email profile offline_access.
    scopes             TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    last_used_at       INTEGER,
    last_used_protocol TEXT,
    last_used_ip       TEXT
);
CREATE INDEX oauth_grants_account ON oauth_grants (account_id);

-- Authorization codes live for two minutes and work once. The PKCE challenge (S256) is required.
CREATE TABLE oauth_codes (
    code_hash      BLOB PRIMARY KEY,
    client_id      INTEGER NOT NULL REFERENCES oauth_clients (id) ON DELETE CASCADE,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    redirect_uri   TEXT NOT NULL,
    scopes         TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    nonce          TEXT,
    auth_time      INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL
);

-- Access tokens (an hour) and refresh tokens (90 days after their last use). A refresh token is
-- used once: trading it in marks it `used_at` and hands out a new one. The same token coming back
-- after that means it was copied, and the whole grant ends (RFC 9700 section 4.14).
CREATE TABLE oauth_tokens (
    token_hash BLOB PRIMARY KEY,
    grant_id   INTEGER NOT NULL REFERENCES oauth_grants (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('access', 'refresh')),
    expires_at INTEGER NOT NULL,
    used_at    INTEGER
);
CREATE INDEX oauth_tokens_grant ON oauth_tokens (grant_id);

-- What a person agreed an app may do, so signing in again does not ask again. Forgotten when the
-- person revokes the app.
CREATE TABLE oauth_consents (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    client_id  INTEGER NOT NULL REFERENCES oauth_clients (id) ON DELETE CASCADE,
    scopes     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, client_id)
);

-- Logins at another OpenID Connect provider (Authentik, Keycloak, …) that belong to an account
-- here, by the provider's issuer and its lasting subject identifier.
CREATE TABLE external_identities (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    issuer        TEXT NOT NULL,
    subject       TEXT NOT NULL,
    email         TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER,
    UNIQUE (issuer, subject)
);
CREATE INDEX external_identities_account ON external_identities (account_id);

-- Where an account's password is checked: 'local' (the hash here), 'ldap' (a bind at the
-- directory) or 'oidc' (created by a login at another provider; it has no password here).
ALTER TABLE accounts ADD COLUMN auth_source TEXT NOT NULL DEFAULT 'local';
