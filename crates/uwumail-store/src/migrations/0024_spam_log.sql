-- What the spam filter decided about a message, so an admin can look it up afterwards.
--
-- The cases worth looking up are the ones that leave no other trace: refused, greylisted, turned
-- away by DMARC or by a sender list. Those never reach a mailbox and until now lived only in the
-- running server's log, which a restart empties.
--
-- One row per message, not per recipient: a mail to five people is one decision, and the
-- recipients ride along as JSON with what happened to each of them.
--
-- This is the most telling table in the product. It says who writes to whom. Bodies, attachments
-- and link targets never come near it, and the subject of mail that was delivered normally is only
-- kept when an admin switches that on.

CREATE TABLE spam_log (
    id            INTEGER PRIMARY KEY,
    at            INTEGER NOT NULL,
    -- What the server called the message in its own log and in the 250 it answered with.
    smtp_id       TEXT NOT NULL,
    -- The Message-ID header, for finding the same mail in someone else's log.
    message_id    TEXT,
    -- delivered, junk, greylist, reject, dmarc or blocked.
    action        TEXT NOT NULL,
    envelope_from TEXT NOT NULL,
    header_from   TEXT NOT NULL,
    subject       TEXT,
    client_ip     TEXT NOT NULL,
    helo          TEXT NOT NULL,
    -- The sending server's name, when it has one that checks out.
    reverse_name  TEXT,
    size          INTEGER NOT NULL,
    score         REAL,
    -- Every rule that fired: [{"rule": …, "points": …, "detail": …}]
    hits          TEXT NOT NULL,
    -- The Authentication-Results line in full, which says what SPF, DKIM and DMARC found.
    auth          TEXT,
    -- Links the entry to what the person said later with Spam / Not spam (spam_verdicts.junk).
    blob_hash     TEXT,
    -- [{"address": …, "action": …, "mailbox": …}]
    recipients    TEXT NOT NULL
);

CREATE INDEX spam_log_recent ON spam_log (id DESC);
CREATE INDEX spam_log_action ON spam_log (action, id DESC);
-- Clearing out by age.
CREATE INDEX spam_log_at ON spam_log (at);
