-- Masked addresses (docs/jmap-masked-email.md): random addresses a person makes for one website
-- each, so they can see who passed an address on and turn it off. They follow Fastmail's
-- MaskedEmail: 'pending' until the first message arrives (and gone after a day without one),
-- 'enabled', 'disabled' (mail is taken and filed into the Trash without a word) and 'deleted'
-- (mail is refused). A deleted address is never handed out again, so its row stays for good, even
-- after the account it belonged to is gone.
CREATE TABLE masked_addresses (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER REFERENCES accounts (id) ON DELETE SET NULL,
    local_part      TEXT NOT NULL,
    domain_id       INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    state           TEXT NOT NULL DEFAULT 'pending'
                    CHECK (state IN ('pending', 'enabled', 'disabled', 'deleted')),
    for_domain      TEXT NOT NULL DEFAULT '',
    description     TEXT NOT NULL DEFAULT '',
    url             TEXT,
    email_prefix    TEXT,
    created_by      TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    last_message_at INTEGER,
    created_modseq  INTEGER NOT NULL DEFAULT 0,
    updated_modseq  INTEGER NOT NULL DEFAULT 0,
    UNIQUE (local_part, domain_id)
);
CREATE INDEX masked_addresses_account ON masked_addresses (account_id);

-- Which domains people may make masked addresses on. Off everywhere until an admin opens one.
ALTER TABLE domains ADD COLUMN masked_addresses INTEGER NOT NULL DEFAULT 0;
