-- Outgoing mail another server did not take. The queue forgets a message once nobody is waiting
-- for it any more, and "many messages were refused" is no use to an admin who cannot see which.
-- Who wrote to whom and what the other side said, like the queue shows; never subjects or content.
CREATE TABLE refused_deliveries (
    id        INTEGER PRIMARY KEY,
    at        INTEGER NOT NULL,
    sender    TEXT NOT NULL,              -- the return path, empty for bounces
    recipient TEXT NOT NULL,
    error     TEXT NOT NULL,
    gave_up   INTEGER NOT NULL DEFAULT 0  -- 1: put off until the message expired, 0: refused outright
);
CREATE INDEX refused_deliveries_at ON refused_deliveries (at);
