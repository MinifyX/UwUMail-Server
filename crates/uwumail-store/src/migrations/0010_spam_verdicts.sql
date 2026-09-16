-- How each delivered message was counted for its sender's reputation, so a person marking it as
-- spam or not spam moves that one count instead of adding another. Keyed by the stored message,
-- which all recipients of one delivery share.
CREATE TABLE spam_verdicts (
    blob_hash  TEXT PRIMARY KEY,
    subject    TEXT NOT NULL,     -- the spam_reputation row it counts for
    junk       INTEGER NOT NULL,  -- 1 while it counts as junk
    counted_at INTEGER NOT NULL
);
CREATE INDEX spam_verdicts_counted ON spam_verdicts (counted_at);
