-- Ids that stand for a person or a credential are never handed out twice (security audit 0.16.0,
-- STORE-1 and STORE-4).
--
-- `accounts`, `app_passwords` and `oauth_grants` were plain `INTEGER PRIMARY KEY` tables, so
-- SQLite gave a new row the largest id plus one: after the newest account was purged, the next
-- account got its id. Whatever still pointed at the old id then pointed at the new person — an
-- IMAP, ManageSieve or JMAP WebSocket connection that stayed open, the spam filter's learned words
-- (`bayes_*`, which have no foreign key), a push subscription made with `app:<id>` or
-- `oauth:<id>`.
--
-- Rebuilding these tables with AUTOINCREMENT would mean dropping `accounts` under foreign keys
-- that cascade into every table, so instead each keeps its highest id ever here. New rows take
-- the next one (`db::next_id`), and the triggers refuse any other id, including one SQLite would
-- pick by itself.
CREATE TABLE id_high_water (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
) WITHOUT ROWID;

-- The highest id in use, or still named somewhere after its row went.
INSERT INTO id_high_water (name, value) VALUES
    ('accounts', max(
        coalesce((SELECT max(id) FROM accounts), 0),
        coalesce((SELECT max(account_id) FROM bayes_totals), 0),
        coalesce((SELECT max(account_id) FROM bayes_tokens), 0),
        coalesce((SELECT max(account_id) FROM bayes_queue), 0)
    )),
    ('app_passwords', max(
        coalesce((SELECT max(id) FROM app_passwords), 0),
        coalesce((SELECT max(CAST(substr(credential, 5) AS INTEGER)) FROM push_subscriptions
                  WHERE credential LIKE 'app:%'), 0)
    )),
    ('oauth_grants', max(
        coalesce((SELECT max(id) FROM oauth_grants), 0),
        coalesce((SELECT max(CAST(substr(credential, 7) AS INTEGER)) FROM push_subscriptions
                  WHERE credential LIKE 'oauth:%'), 0)
    ));

CREATE TRIGGER accounts_id_never_reused BEFORE INSERT ON accounts
WHEN NEW.id <= (SELECT value FROM id_high_water WHERE name = 'accounts')
BEGIN
    SELECT RAISE(ABORT, 'account ids are never reused');
END;
CREATE TRIGGER accounts_id_high_water AFTER INSERT ON accounts
BEGIN
    UPDATE id_high_water SET value = NEW.id WHERE name = 'accounts' AND value < NEW.id;
END;

CREATE TRIGGER app_passwords_id_never_reused BEFORE INSERT ON app_passwords
WHEN NEW.id <= (SELECT value FROM id_high_water WHERE name = 'app_passwords')
BEGIN
    SELECT RAISE(ABORT, 'app password ids are never reused');
END;
CREATE TRIGGER app_passwords_id_high_water AFTER INSERT ON app_passwords
BEGIN
    UPDATE id_high_water SET value = NEW.id WHERE name = 'app_passwords' AND value < NEW.id;
END;

CREATE TRIGGER oauth_grants_id_never_reused BEFORE INSERT ON oauth_grants
WHEN NEW.id <= (SELECT value FROM id_high_water WHERE name = 'oauth_grants')
BEGIN
    SELECT RAISE(ABORT, 'OAuth grant ids are never reused');
END;
CREATE TRIGGER oauth_grants_id_high_water AFTER INSERT ON oauth_grants
BEGIN
    UPDATE id_high_water SET value = NEW.id WHERE name = 'oauth_grants' AND value < NEW.id;
END;

-- What purged accounts left behind: learned words have no foreign key (0 is the whole server's).
DELETE FROM bayes_tokens WHERE account_id != 0 AND account_id NOT IN (SELECT id FROM accounts);
DELETE FROM bayes_totals WHERE account_id != 0 AND account_id NOT IN (SELECT id FROM accounts);
DELETE FROM bayes_learned WHERE account_id != 0 AND account_id NOT IN (SELECT id FROM accounts);
DELETE FROM bayes_queue WHERE account_id != 0 AND account_id NOT IN (SELECT id FROM accounts);

-- Push subscriptions whose app password or OAuth sign-in is gone. They were never pushed to, but
-- stayed until the next clean-up, where an id handed out again could have woken them.
DELETE FROM push_subscriptions
 WHERE (credential LIKE 'app:%' AND NOT EXISTS (SELECT 1 FROM app_passwords ap
            WHERE ap.id = CAST(substr(credential, 5) AS INTEGER) AND ap.account_id = push_subscriptions.account_id))
    OR (credential LIKE 'oauth:%' AND NOT EXISTS (SELECT 1 FROM oauth_grants g
            WHERE g.id = CAST(substr(credential, 7) AS INTEGER) AND g.account_id = push_subscriptions.account_id));

-- Which login held a submission back (JMAP's undo window and send later), named like a push
-- subscription's `credential`. Held mail stays unsent when that app password or OAuth app is
-- revoked, the password changes, or the account is disabled or moved to the trash (security audit
-- 0.16.0, PROTOCOLS-10). NULL, for what was held before, counts as the account password.
ALTER TABLE email_submissions ADD COLUMN credential TEXT;
