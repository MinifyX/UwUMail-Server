-- Whether this account may only send to addresses this server takes itself (docs/groups.md).
--
-- Its own column and not a protocol switch, like the webmail (migration 0028): SMTP says whether
-- the account sends at all, this says how far. It holds for everything that leaves in the account's
-- name, over any door, and only an admin sets it.
--
-- Nobody has it today, so nothing changes for anyone who exists.
ALTER TABLE accounts ADD COLUMN internal_only INTEGER NOT NULL DEFAULT 0;
