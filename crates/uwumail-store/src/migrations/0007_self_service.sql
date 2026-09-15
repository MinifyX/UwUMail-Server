-- What people set up for their own mailbox: forwarding and their own aliases.

-- Forwarding: mail for a person also goes to these addresses once they are confirmed.
ALTER TABLE accounts ADD COLUMN forward_keep_copy INTEGER NOT NULL DEFAULT 1;
-- An admin can forbid forwarding to other servers for one person.
ALTER TABLE accounts ADD COLUMN external_forwarding_blocked INTEGER NOT NULL DEFAULT 0;
CREATE TABLE forward_targets (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    address      TEXT NOT NULL,
    token_hash   BLOB UNIQUE,  -- confirmation link for addresses on other servers
    created_at   INTEGER NOT NULL,
    confirmed_at INTEGER,      -- NULL until the owner of the address agreed
    UNIQUE (account_id, address)
);

-- Aliases people create themselves: allowed per domain, limited per person.
ALTER TABLE domains ADD COLUMN self_service_aliases INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN alias_limit INTEGER NOT NULL DEFAULT 10;
ALTER TABLE addresses ADD COLUMN created_by_owner INTEGER NOT NULL DEFAULT 0;
-- An alias its owner deleted stays theirs for 30 days, so late mail cannot reach someone else.
CREATE TABLE released_addresses (
    local_part  TEXT NOT NULL,
    domain_id   INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    released_at INTEGER NOT NULL,
    PRIMARY KEY (local_part, domain_id)
);
