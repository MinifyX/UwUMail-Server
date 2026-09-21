-- Settings the webmail and the UwUMail apps keep in sync for an account (docs/jmap-settings.md).
--
-- One row per key. Lists are kept as one key per entry, so two devices adding to the same list at
-- the same time never overwrite each other. The value is JSON, checked against the key's rules
-- before it gets here.
--
-- The settings the portal already keeps (theme, tone, language and the webmail's mail choices) do
-- not live here but in accounts.preferences, so there is one place for each of them.
CREATE TABLE user_settings (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (account_id, key)
) WITHOUT ROWID;

-- The account's change number at the last write to its settings, from here or from the portal:
-- the JMAP state of UserSettings. 0 means never written.
ALTER TABLE accounts ADD COLUMN settings_modseq INTEGER NOT NULL DEFAULT 0;
