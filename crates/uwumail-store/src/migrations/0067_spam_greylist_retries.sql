-- Retries of a greylisted message are one waiting message, not one row per attempt.
--
-- Greylisting asks the sender to come back, and well-behaved servers come back several times
-- before the greylist window opens. Each attempt used to be kept as a row of its own, so the list
-- showed the same mail three or four times, and delivering one of them left the others behind.
--
-- A retry now bumps the row it repeats: how often the sender tried, and when it last did.

ALTER TABLE greylist_hold ADD COLUMN attempts INTEGER NOT NULL DEFAULT 1;
ALTER TABLE greylist_hold ADD COLUMN last_at INTEGER;
