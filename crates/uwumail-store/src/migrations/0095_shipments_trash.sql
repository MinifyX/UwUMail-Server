-- 0.37.2 reads mail in the Trash for parcels too. The look back 0.37.1 started skipped it, so the
-- last 14 days are read once more; a mail already noted changes nothing.
INSERT INTO shipment_rescans (account_id, since, started_at)
    SELECT id, CAST(strftime('%s', 'now') AS INTEGER) - 14 * 86400, CAST(strftime('%s', 'now') AS INTEGER)
    FROM accounts WHERE deleted_at IS NULL
    ON CONFLICT (account_id) DO UPDATE SET since = min(since, excluded.since), after_id = 0;
