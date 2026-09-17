-- People who may send as any address of a domain, e.g. for a shared office inbox. Admins decide.
CREATE TABLE send_as_domains (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    domain_id  INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, domain_id)
);
