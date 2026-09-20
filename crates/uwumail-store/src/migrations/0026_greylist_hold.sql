-- Greylisted mail, kept until its sender comes back, so the person it was meant for can decide
-- about it instead of waiting in the dark.
--
-- Greylisting answers a suspicious sender with "try again later". Well-behaved servers do, and the
-- mail arrives a few minutes on; spammers usually never return. That works, but it means a message
-- a person was waiting for can sit somewhere invisible for a quarter of an hour, and one that never
-- comes back leaves no trace they could look at at all.
--
-- So the message is kept here while its sender is being asked to come back. The portal shows the
-- sender and the subject and nothing else — never the body, never a link, never an attachment. Who
-- wants to read it delivers it first and reads it in their mailbox, where mail belongs.
--
-- One row per recipient account, not per message: two people greylisted on the same mail each
-- decide for themselves, and each only ever sees their own row.
--
-- Unlike spam_log this holds a whole message, which is why no admin route ever reads it. The mail
-- belongs to the person it was addressed to, and the person it was addressed to is the only one who
-- gets to see it.

CREATE TABLE greylist_hold (
    id            INTEGER PRIMARY KEY,
    at            INTEGER NOT NULL,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The address the mail was sent to, which may be an alias of the account.
    address       TEXT NOT NULL,
    envelope_from TEXT NOT NULL,
    header_from   TEXT NOT NULL,
    subject       TEXT,
    message_id    TEXT,
    -- What the server called the message in its own log.
    smtp_id       TEXT NOT NULL,
    client_ip     TEXT NOT NULL,
    score         REAL,
    size          INTEGER NOT NULL,
    -- The message as it would have been delivered, Received and spam headers and all. Emptied once
    -- the row is settled, which is what frees the blob.
    blob_hash     TEXT REFERENCES blobs (hash),
    -- What the sending server handed us, before our own headers went on top. A retry of the same
    -- message hashes to the same value, which is how a settled row recognises it coming back.
    raw_hash      TEXT NOT NULL,
    -- When this stops being interesting: the same moment the greylist entry itself is forgotten.
    expires_at    INTEGER NOT NULL,
    -- NULL while it waits. 'delivered' or 'discarded' once the person decided, and from then on the
    -- row is a tombstone: it keeps the retry from arriving a second time, or from undoing a discard.
    settled       TEXT,
    settled_at    INTEGER
);

CREATE INDEX greylist_hold_waiting ON greylist_hold (account_id, id DESC) WHERE settled IS NULL;
-- What a retry is looked up by.
CREATE INDEX greylist_hold_return ON greylist_hold (account_id, raw_hash);
CREATE INDEX greylist_hold_message ON greylist_hold (account_id, message_id) WHERE message_id IS NOT NULL;
-- Clearing out by age.
CREATE INDEX greylist_hold_expiry ON greylist_hold (expires_at);

-- Blob reference counting. Settling a row sets blob_hash to NULL, so the update needs a trigger of
-- its own; without it the message would stay on disk with nothing pointing at it.
CREATE TRIGGER greylist_hold_blob_ref AFTER INSERT ON greylist_hold
WHEN NEW.blob_hash IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash;
END;

CREATE TRIGGER greylist_hold_blob_unref AFTER DELETE ON greylist_hold
WHEN OLD.blob_hash IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash;
END;

CREATE TRIGGER greylist_hold_blob_moved AFTER UPDATE OF blob_hash ON greylist_hold
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash AND OLD.blob_hash IS NOT NULL;
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash AND NEW.blob_hash IS NOT NULL;
END;
