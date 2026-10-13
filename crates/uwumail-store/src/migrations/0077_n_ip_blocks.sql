-- Networks kept out after a login that cannot be anyone's here (limiter.rs), so a restart does not
-- let them back in. A row stays for a while after `until`: coming back then is blocked for longer.
CREATE TABLE ip_blocks (
    network    TEXT PRIMARY KEY, -- the address, or the /64 of an IPv6 one
    ip         TEXT NOT NULL,    -- the address that did it
    reason     TEXT NOT NULL,
    login      TEXT NOT NULL DEFAULT '',
    protocol   TEXT NOT NULL DEFAULT '',
    blocked_at INTEGER NOT NULL,
    until      INTEGER NOT NULL,
    times      INTEGER NOT NULL DEFAULT 1
);

-- Networks somebody logged in from lately. One bad login from these is counted, not blocked.
CREATE INDEX ip_blocks_until ON ip_blocks (until);

CREATE TABLE known_networks (
    network   TEXT PRIMARY KEY,
    last_seen INTEGER NOT NULL
);
