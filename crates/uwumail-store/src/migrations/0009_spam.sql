-- Spam filtering: greylisting for suspicious senders and how a sender behaved so far.

-- Senders we asked to come back later. Only suspicious mail lands here, so a first message from a
-- well-behaved server is never delayed. The triplet is the classic one, with the client address
-- reduced to its network so that a sending pool retrying from a neighbour still matches.
CREATE TABLE spam_greylist (
    network    TEXT NOT NULL,     -- IPv4 /24 or IPv6 /64 of the sending server
    sender     TEXT NOT NULL,     -- envelope sender, empty for bounces
    recipient  TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL,
    passed_at  INTEGER,           -- when the retry was let through; NULL while still waiting
    PRIMARY KEY (network, sender, recipient)
);
CREATE INDEX spam_greylist_last_seen ON spam_greylist (last_seen);

-- What a sender delivered so far: mail that arrived in an inbox against mail that the filter or a
-- person put into Junk. Keyed by 'domain:example.com' for senders that pass DMARC, otherwise by
-- 'network:192.0.2.0/24', because only then the domain means anything.
CREATE TABLE spam_reputation (
    subject    TEXT PRIMARY KEY,
    good       INTEGER NOT NULL DEFAULT 0,
    junk       INTEGER NOT NULL DEFAULT 0,
    first_seen INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX spam_reputation_updated ON spam_reputation (updated_at);
