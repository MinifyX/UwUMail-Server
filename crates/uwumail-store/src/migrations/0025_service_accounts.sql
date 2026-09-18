-- Service accounts: a mailbox that belongs to a program instead of a person. It never signs in to
-- the portal -- app passwords are the only way in -- and each protocol is switched on by itself.
--
-- Everywhere above this file a service is a third role next to admin and user. Here it is a column
-- of its own, because SQLite cannot widen the CHECK on accounts.role without rebuilding the table,
-- and rebuilding the one table half the database points at, with cascades armed, is not a risk
-- worth taking for a spelling. A service always stores role = 'user'.
ALTER TABLE accounts ADD COLUMN kind TEXT NOT NULL DEFAULT 'person' CHECK (kind IN ('person', 'service'));

-- Which protocols this account may use at all, whoever holds its password. A person has all of
-- them, so every account that exists today keeps what it had.
ALTER TABLE accounts ADD COLUMN smtp_enabled    INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN imap_enabled    INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN jmap_enabled    INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN caldav_enabled  INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN carddav_enabled INTEGER NOT NULL DEFAULT 1;

-- Where mail goes for a service that has no mailbox of its own (neither IMAP nor JMAP). Empty
-- means its address refuses mail at the door, which is what a send-only service should do.
ALTER TABLE accounts ADD COLUMN redirect_to TEXT NOT NULL DEFAULT '';
