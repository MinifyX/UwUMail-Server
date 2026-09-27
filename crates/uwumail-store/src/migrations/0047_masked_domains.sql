-- Masked-only domains and who may make masked addresses where (docs/jmap-masked-email.md).
--
-- A domain is either a mail domain ('mail', everything as before) or a domain only for masked
-- addresses ('masked'): no people, aliases, groups, forwarding addresses or catch-all there.
ALTER TABLE domains ADD COLUMN kind TEXT NOT NULL DEFAULT 'mail' CHECK (kind IN ('mail', 'masked'));

-- The policy of a mail domain for its users (accounts whose login is on it): masked addresses
-- 'off', on the domain itself ('own'), on the masked-only domains of domain_masked_domains
-- ('dedicated') or both. The default is the domain taken when a client names none; NULL picks
-- one by itself.
ALTER TABLE domains ADD COLUMN masked_mode TEXT NOT NULL DEFAULT 'off'
    CHECK (masked_mode IN ('off', 'own', 'dedicated', 'both'));
ALTER TABLE domains ADD COLUMN masked_default_domain_id INTEGER REFERENCES domains (id) ON DELETE SET NULL;

CREATE TABLE domain_masked_domains (
    domain_id        INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    masked_domain_id INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    PRIMARY KEY (domain_id, masked_domain_id)
) WITHOUT ROWID;

-- A person's own policy, part by part: NULL (or masked_domains_custom = 0) means as their domain.
ALTER TABLE accounts ADD COLUMN masked_mode TEXT
    CHECK (masked_mode IS NULL OR masked_mode IN ('off', 'own', 'dedicated', 'both'));
ALTER TABLE accounts ADD COLUMN masked_domains_custom INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN masked_default_domain_id INTEGER REFERENCES domains (id) ON DELETE SET NULL;

CREATE TABLE account_masked_domains (
    account_id       INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    masked_domain_id INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    PRIMARY KEY (account_id, masked_domain_id)
) WITHOUT ROWID;
CREATE INDEX account_masked_domains_domain ON account_masked_domains (masked_domain_id);

-- A domain that was open for masked addresses lets its own users make them there, as before.
-- Users of other domains no longer can; the masked addresses they made keep working.
UPDATE domains SET masked_mode = 'own' WHERE masked_addresses = 1;
ALTER TABLE domains DROP COLUMN masked_addresses;
