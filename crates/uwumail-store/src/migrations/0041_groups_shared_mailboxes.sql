-- Groups (docs/groups.md): an address of one of our domains that delivers to several people here,
-- such as info@ or vorstand@ of a club. Each member gets the message the way mail for their own
-- address arrives: their own spam decision, sender lists, rules and quota.
--
-- who_may_send says who may write to the group: 'anyone', only its 'members', or only addresses of
-- the group's own 'domain'. members_may_send_as lets the members send with the group's address.
CREATE TABLE groups (
    id                  INTEGER PRIMARY KEY,
    local_part          TEXT NOT NULL,
    domain_id           INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    name                TEXT NOT NULL DEFAULT '',
    who_may_send        TEXT NOT NULL DEFAULT 'anyone' CHECK (who_may_send IN ('anyone', 'members', 'domain')),
    members_may_send_as INTEGER NOT NULL DEFAULT 0,
    created_at          INTEGER NOT NULL,
    UNIQUE (local_part, domain_id)
);

CREATE TABLE group_members (
    group_id   INTEGER NOT NULL REFERENCES groups (id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, account_id)
);
CREATE INDEX group_members_account ON group_members (account_id);

-- Shared mailboxes: a mailbox several people use, such as support@. It is an account of its own
-- (stored as a service, kind = 'service') that nobody signs in to: no password, no app passwords,
-- no portal. Its members reach every one of its folders, new ones included, as if each were
-- shared with them (docs/sharing.md), and those with may_send may send with its address.
ALTER TABLE accounts ADD COLUMN shared_mailbox INTEGER NOT NULL DEFAULT 0;

CREATE TABLE shared_mailbox_members (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    member_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    rights     TEXT NOT NULL DEFAULT 'lrswipkxtea' CHECK (rights <> ''),
    may_send   INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, member_id),
    CHECK (account_id <> member_id)
);
CREATE INDEX shared_mailbox_members_member ON shared_mailbox_members (member_id);
