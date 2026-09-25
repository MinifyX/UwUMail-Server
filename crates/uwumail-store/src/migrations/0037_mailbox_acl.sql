-- Folders shared between people on this server (docs/sharing.md): who else may use a mailbox and
-- how, as RFC 4314 rights letters in their usual order ("lrswipkxtea"). The owner is not listed:
-- they always have every right on their own mailboxes. Deleting a mailbox or either account
-- removes the entry with it.
CREATE TABLE mailbox_acl (
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes (id) ON DELETE CASCADE,
    owner_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    grantee_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    rights     TEXT NOT NULL CHECK (rights <> ''),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (mailbox_id, grantee_id),
    CHECK (owner_id <> grantee_id)
);
CREATE INDEX mailbox_acl_grantee ON mailbox_acl (grantee_id);
CREATE INDEX mailbox_acl_owner ON mailbox_acl (owner_id);
