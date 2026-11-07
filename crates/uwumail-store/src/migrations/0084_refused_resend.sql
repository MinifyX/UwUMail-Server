-- A refused mail from one of our accounts keeps its message, so the admin can send it again once
-- the cause is fixed (a block list lifted, a typo in DNS). Bounces and forwards keep nothing.
ALTER TABLE refused_deliveries ADD COLUMN blob_hash TEXT REFERENCES blobs (hash);
ALTER TABLE refused_deliveries ADD COLUMN account_id INTEGER REFERENCES accounts (id) ON DELETE SET NULL;
ALTER TABLE refused_deliveries ADD COLUMN env_id TEXT;
ALTER TABLE refused_deliveries ADD COLUMN notify_flags INTEGER NOT NULL DEFAULT 0;
ALTER TABLE refused_deliveries ADD COLUMN orcpt TEXT;
ALTER TABLE refused_deliveries ADD COLUMN lifetime INTEGER NOT NULL DEFAULT 0; -- the queue lifetime it had

CREATE TRIGGER refused_blob_ref AFTER INSERT ON refused_deliveries WHEN NEW.blob_hash IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.blob_hash;
END;

CREATE TRIGGER refused_blob_unref AFTER DELETE ON refused_deliveries WHEN OLD.blob_hash IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.blob_hash;
END;
