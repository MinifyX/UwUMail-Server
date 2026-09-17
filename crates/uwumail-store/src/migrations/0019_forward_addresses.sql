-- Addresses of our domains without a mailbox of their own: mail for them goes straight on to other
-- addresses, here or elsewhere. Set up by admins, so the targets need no confirmation.
CREATE TABLE forward_addresses (
    id         INTEGER PRIMARY KEY,
    local_part TEXT NOT NULL,
    domain_id  INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    targets    TEXT NOT NULL,     -- normalized addresses, one per line
    note       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE (local_part, domain_id)
);
