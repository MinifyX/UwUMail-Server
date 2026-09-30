-- When what the password stands for last changed: a new password, a second factor, the rule that
-- apps need app passwords, a different way to check passwords. Push subscriptions and connections
-- made with the password end with it (`push::STILL_VALID`).
--
-- Until now they went by `credentials_changed_at`, which also moves when an app password is
-- revoked, so revoking one app password ended the password's subscriptions and connections too
-- whenever they were older than that second. `credentials_changed_at` stays what the login cache
-- goes by. Existing accounts start from it, which keeps what already ended ended.
ALTER TABLE accounts ADD COLUMN password_changed_at INTEGER NOT NULL DEFAULT 0;
UPDATE accounts SET password_changed_at = credentials_changed_at;
