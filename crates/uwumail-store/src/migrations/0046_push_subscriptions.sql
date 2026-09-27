-- Web Push subscriptions (JMAP PushSubscription, RFC 8620 section 7.2; docs/jmap-push.md): an
-- address at a push service (a browser vendor's, or a UnifiedPush distributor) the server POSTs a
-- StateChange to when something changes in the account, so an app learns of new mail while it is
-- closed.
--
-- A subscription belongs to the login that made it (`credential`): `session:<hash>` for the
-- webmail's session, `app:<id>` for an app password, `password` for the account password. It ends
-- with that login: when the session ends, the app password goes, or the password changes.
--
-- The address is a credential by itself (whoever has it can push to the device) and the auth
-- secret keeps the content from the push service, so both are sealed like the passwords of
-- fetched mailboxes (0027). `url_shown` is the host, for the log. Nothing is sent to a new
-- subscription but its verification code until the client has sent that code back (`verified`).
CREATE TABLE push_subscriptions (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    credential        TEXT NOT NULL,
    device_client_id  TEXT NOT NULL,
    url               BLOB NOT NULL,
    -- SHA-256 of the address: a second subscription to the same address replaces the first once
    -- it is verified, without unsealing anything.
    url_digest        TEXT NOT NULL,
    url_shown         TEXT NOT NULL,
    -- The device's P-256 public key and sealed auth secret (RFC 8291); both NULL without encryption.
    keys_p256dh       TEXT,
    keys_auth         BLOB,
    verification_code TEXT NOT NULL,
    verified          INTEGER NOT NULL DEFAULT 0,
    -- Wrong codes sent back; after a few the subscription is gone.
    verify_attempts   INTEGER NOT NULL DEFAULT 0,
    expires           INTEGER NOT NULL,
    -- A JSON array of type names, or NULL for all of them.
    types             TEXT,
    created_at        INTEGER NOT NULL,
    last_ok_at        INTEGER,
    -- Failed pushes in a row; the next one waits until `retry_at`, and too many end the subscription.
    failures          INTEGER NOT NULL DEFAULT 0,
    retry_at          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX push_subscriptions_account ON push_subscriptions (account_id, verified);
CREATE INDEX push_subscriptions_expires ON push_subscriptions (expires);
