-- The shop's own tracking page next to the carrier's, from authenticated mail only. The last 14
-- days are read once more so dates ending a sentence and "ist fast bei dir" count.
ALTER TABLE shipments ADD COLUMN shop_tracking_url TEXT;
INSERT INTO shipment_rescans (account_id, since, started_at)
    SELECT id, CAST(strftime('%s', 'now') AS INTEGER) - 14 * 86400, CAST(strftime('%s', 'now') AS INTEGER)
    FROM accounts WHERE deleted_at IS NULL
    ON CONFLICT (account_id) DO UPDATE SET since = min(since, excluded.since), after_id = 0;
