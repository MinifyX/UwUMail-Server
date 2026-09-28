-- Profile pictures (docs/profile-pictures.md): one per account (people, services, shared
-- mailboxes), one per group and one logo per domain, which stands in for the domain's addresses
-- that have none. Stored as the server wrote them anew: square, at most 512 × 512, no metadata.
-- `face` is the same picture as the 48 × 48 PNG of the `Face:` header, for accounts only. `hash` is
-- the SHA-256 of `data`: the JMAP blob id and the ETag.
CREATE TABLE profile_pictures (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER UNIQUE REFERENCES accounts (id) ON DELETE CASCADE,
    group_id   INTEGER UNIQUE REFERENCES groups (id) ON DELETE CASCADE,
    domain_id  INTEGER UNIQUE REFERENCES domains (id) ON DELETE CASCADE,
    media_type TEXT NOT NULL,
    data       BLOB NOT NULL,
    face       BLOB,
    hash       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((account_id IS NOT NULL) + (group_id IS NOT NULL) + (domain_id IS NOT NULL) = 1)
);

-- Who sees an account's or a group's picture: nobody ('off'), people of this server ('server'), or
-- everyone, over Libravatar and the Face header too ('public'). send_face puts the Face header on
-- the person's mail while the picture is public. picture_modseq is the state of JMAP's
-- ProfilePicture.
ALTER TABLE accounts ADD COLUMN picture_visibility TEXT NOT NULL DEFAULT 'server'
    CHECK (picture_visibility IN ('off', 'server', 'public'));
ALTER TABLE accounts ADD COLUMN send_face INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN picture_modseq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE groups ADD COLUMN picture_visibility TEXT NOT NULL DEFAULT 'server'
    CHECK (picture_visibility IN ('off', 'server', 'public'));

-- Whether pictures of the domain's addresses may be public at all. The whole server has the same
-- switch as the setting `pictures.public` ('false' forbids); both have to allow it.
ALTER TABLE domains ADD COLUMN public_pictures INTEGER NOT NULL DEFAULT 1;

-- Which cards with a photo name which address, so a sender's picture is found without reading every
-- vCard of an account: one row per card and address, kept with every write of a card over CardDAV
-- and JMAP. `account_id` is the address book's owner; cards shared with someone are found through
-- dav_shares. Filled for the cards already there when the server starts after this migration.
CREATE TABLE contact_photos (
    resource_id   INTEGER NOT NULL REFERENCES dav_resources (id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL,
    account_id    INTEGER NOT NULL,
    email         TEXT NOT NULL,
    PRIMARY KEY (resource_id, email)
) WITHOUT ROWID;
CREATE INDEX contact_photos_account ON contact_photos (account_id, email);
CREATE INDEX contact_photos_email ON contact_photos (email);
INSERT INTO settings (key, value) VALUES ('contact_photos.backfill', 'pending')
    ON CONFLICT (key) DO UPDATE SET value = excluded.value;

-- The newest Face picture that came with mail from an address, kept only when DMARC passed for the
-- From domain. A bounded cache: the oldest go first.
CREATE TABLE received_faces (
    email       TEXT PRIMARY KEY,
    png         BLOB NOT NULL,
    received_at INTEGER NOT NULL
);
CREATE INDEX received_faces_age ON received_faces (received_at);

-- Counts every change that can change which addresses Libravatar answers for, so the table of
-- their hashes is made anew only then.
CREATE TABLE avatar_version (
    id    INTEGER PRIMARY KEY CHECK (id = 1),
    value INTEGER NOT NULL
);
INSERT INTO avatar_version (id, value) VALUES (1, 1);

CREATE TRIGGER avatar_addresses_added AFTER INSERT ON addresses
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_addresses_changed AFTER UPDATE ON addresses
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_addresses_removed AFTER DELETE ON addresses
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_accounts_changed
AFTER UPDATE OF login, picture_visibility, disabled, deleted_at, kind, shared_mailbox ON accounts
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_accounts_removed AFTER DELETE ON accounts
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_pictures_added AFTER INSERT ON profile_pictures
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_pictures_changed AFTER UPDATE ON profile_pictures
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_pictures_removed AFTER DELETE ON profile_pictures
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_domains_changed AFTER UPDATE OF name, public_pictures ON domains
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_domains_removed AFTER DELETE ON domains
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_groups_added AFTER INSERT ON groups
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_groups_changed AFTER UPDATE ON groups
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_groups_removed AFTER DELETE ON groups
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_setting_added AFTER INSERT ON settings WHEN NEW.key = 'pictures.public'
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_setting_changed AFTER UPDATE ON settings WHEN NEW.key = 'pictures.public'
BEGIN UPDATE avatar_version SET value = value + 1; END;
CREATE TRIGGER avatar_setting_removed AFTER DELETE ON settings WHEN OLD.key = 'pictures.public'
BEGIN UPDATE avatar_version SET value = value + 1; END;
