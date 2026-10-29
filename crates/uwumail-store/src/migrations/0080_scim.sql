-- People and groups pushed by an identity provider over SCIM 2.0 (docs/uwuauth.md). `id` is the SCIM
-- id the provider keeps for an account; ids are made for every person when the provider first looks.
-- `external_id` is the provider's own id of the person, which is also the OpenID Connect subject.
-- `disabled_by_scim`: the provider switched the account off, so only the provider switches it back on.
CREATE TABLE scim_users (
    id TEXT PRIMARY KEY,
    account_id INTEGER NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    external_id TEXT,
    disabled_by_scim INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);
CREATE UNIQUE INDEX scim_users_external ON scim_users(external_id) WHERE external_id IS NOT NULL;

-- Groups are kept so the provider can find them again; they change nothing about mail. `members` is
-- a JSON list of SCIM user ids.
CREATE TABLE scim_groups (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL UNIQUE,
    external_id TEXT,
    members TEXT NOT NULL DEFAULT '[]',
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL
);
