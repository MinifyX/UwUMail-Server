-- Signatures per domain (0.22): a person writes one signature for all their addresses of a domain
-- (or for every domain, '*'), a single address may still have its own, and an admin may set a
-- company signature per domain, as a template or as a footer the server appends on sending.

-- Whether the identity's own signature is used. Before, every identity had only its own one; those
-- with one keep it as their own.
ALTER TABLE identities ADD COLUMN signature_override INTEGER NOT NULL DEFAULT 0;
UPDATE identities SET signature_override = 1 WHERE text_signature <> '' OR html_signature <> '';

-- A person's signature for all their addresses of one domain; `domain` is '*' for every domain.
CREATE TABLE user_signatures (
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    domain         TEXT NOT NULL,
    text_signature TEXT NOT NULL DEFAULT '',
    html_signature TEXT NOT NULL DEFAULT '',
    updated_at     INTEGER NOT NULL,
    PRIMARY KEY (account_id, domain)
) WITHOUT ROWID;

-- The company signature of a domain: a template for people without their own, or a footer the
-- server appends to every message sent from the domain.
CREATE TABLE domain_signatures (
    domain_id      INTEGER PRIMARY KEY REFERENCES domains (id) ON DELETE CASCADE,
    mode           TEXT NOT NULL DEFAULT 'off' CHECK (mode IN ('off', 'template', 'footer')),
    text_signature TEXT NOT NULL DEFAULT '',
    html_signature TEXT NOT NULL DEFAULT '',
    updated_at     INTEGER NOT NULL
);

-- Where all addresses of a domain had the very same signature, it becomes the domain's.
INSERT INTO user_signatures (account_id, domain, text_signature, html_signature, updated_at)
SELECT i.account_id, lower(substr(i.email, instr(i.email, '@') + 1)), i.text_signature, i.html_signature,
       CAST(strftime('%s', 'now') AS INTEGER)
FROM identities i
WHERE i.signature_override = 1 AND instr(i.email, '@') > 0
GROUP BY i.account_id, lower(substr(i.email, instr(i.email, '@') + 1))
HAVING count(DISTINCT i.text_signature || char(0) || i.html_signature) = 1
   AND count(*) = (SELECT count(*) FROM identities o
                   WHERE o.account_id = i.account_id
                     AND lower(substr(o.email, instr(o.email, '@') + 1)) = lower(substr(i.email, instr(i.email, '@') + 1)));

UPDATE identities SET signature_override = 0, text_signature = '', html_signature = ''
WHERE signature_override = 1 AND EXISTS (
    SELECT 1 FROM user_signatures u
    WHERE u.account_id = identities.account_id
      AND u.domain = lower(substr(identities.email, instr(identities.email, '@') + 1))
      AND u.text_signature = identities.text_signature AND u.html_signature = identities.html_signature);
