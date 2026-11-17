-- 0.38.2: a status word in a promise („Sobald deine Bestellung versendet wird …“) made an order
-- confirmation „Versendet“, and a status never goes back. The same as 0099 once more.
CREATE TEMP TABLE shipments_reread AS
    SELECT s.id, s.account_id, row_number() OVER (PARTITION BY s.account_id ORDER BY s.id) AS n,
           (SELECT min(e.at) FROM shipment_events e WHERE e.shipment_id = s.id) AS first_at
    FROM shipments s
    WHERE s.title IS NULL AND s.hidden = 0
      AND NOT EXISTS (SELECT 1 FROM shipment_events e
                      WHERE e.shipment_id = s.id AND e.source NOT IN ('mail', 'carrier'))
      -- Only cards whose every mail is still there to read again, outside Junk.
      AND NOT EXISTS (SELECT 1 FROM shipment_events e
                      WHERE e.shipment_id = s.id AND e.source = 'mail'
                        AND (e.email_id IS NULL
                             OR NOT EXISTS (SELECT 1 FROM emails m WHERE m.id = e.email_id)
                             OR EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes b ON b.id = em.mailbox_id
                                        WHERE em.email_id = e.email_id AND b.role = 'junk')));

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
