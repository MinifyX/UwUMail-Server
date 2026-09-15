-- Key rotation: a new key starts inactive ("pending") until its DNS record is published,
-- then signs, and the key it replaces is retired but stays published for a while.
ALTER TABLE dkim_keys ADD COLUMN retired_at INTEGER;
