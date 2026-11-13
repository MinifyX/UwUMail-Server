-- Until when a delivery worker holds a recipient. A lease and a backoff are both a future
-- next_attempt_at, so Retry needs this to leave a recipient alone that is being sent right now.
ALTER TABLE queue_recipients ADD COLUMN leased_until INTEGER NOT NULL DEFAULT 0;
