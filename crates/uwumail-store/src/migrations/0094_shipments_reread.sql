-- 0.37.1 reads shop mail better: „Apple“ and „Apple Store“ are one shop, tickets are no parcels,
-- Amazon's own tracker is kept. Orders without a number that only mail made (no name, not hidden,
-- nothing from an app or a carrier) go, and their mail is read again with the new rules.
CREATE TEMP TABLE shipments_reread AS
    SELECT s.id, s.account_id, row_number() OVER (PARTITION BY s.account_id ORDER BY s.id) AS n,
           (SELECT min(e.at) FROM shipment_events e WHERE e.shipment_id = s.id) AS first_at
    FROM shipments s
    WHERE s.number IS NULL AND s.title IS NULL AND s.hidden = 0
      AND NOT EXISTS (SELECT 1 FROM shipment_events e WHERE e.shipment_id = s.id AND e.source <> 'mail');

-- Apps that synced them learn they are gone (as `destroy` does, one modseq each).
INSERT OR IGNORE INTO changes (account_id, modseq, kind, object_id, change)
    SELECT r.account_id, a.modseq + r.n, 'Shipment', r.id, 'destroyed'
    FROM shipments_reread r JOIN accounts a ON a.id = r.account_id;
UPDATE accounts SET modseq = modseq + (SELECT count(*) FROM shipments_reread r WHERE r.account_id = accounts.id)
    WHERE id IN (SELECT account_id FROM shipments_reread);

INSERT INTO shipment_rescans (account_id, since, started_at)
    SELECT account_id, coalesce(min(first_at), CAST(strftime('%s', 'now') AS INTEGER)), CAST(strftime('%s', 'now') AS INTEGER)
    FROM shipments_reread GROUP BY account_id
    ON CONFLICT (account_id) DO UPDATE SET since = min(since, excluded.since), after_id = 0;

DELETE FROM shipments WHERE id IN (SELECT id FROM shipments_reread);
DROP TABLE shipments_reread;
