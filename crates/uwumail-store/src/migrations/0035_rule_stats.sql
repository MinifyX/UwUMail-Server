-- Sender and word list entries can run out (a block for 30 days), and remember how often and when they
-- last decided something, so entries nobody needs any more can be found and cleaned up.
ALTER TABLE sender_lists ADD COLUMN expires_at INTEGER;       -- Unix seconds; NULL: for good
ALTER TABLE sender_lists ADD COLUMN hits INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sender_lists ADD COLUMN last_hit_at INTEGER;
ALTER TABLE word_entries ADD COLUMN expires_at INTEGER;
ALTER TABLE word_entries ADD COLUMN hits INTEGER NOT NULL DEFAULT 0;
ALTER TABLE word_entries ADD COLUMN last_hit_at INTEGER;
CREATE INDEX sender_lists_expires ON sender_lists (expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX word_entries_expires ON word_entries (expires_at) WHERE expires_at IS NOT NULL;
