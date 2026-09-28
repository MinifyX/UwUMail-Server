-- Alerts the server fires itself (docs/jmap-calendars.md): a push to the apps (CalendarAlert) or a
-- mail. For every event with alerts, each account that sees it keeps the next time each alert
-- goes off; after a change of the event, or of what the account keeps for itself about it, the
-- pair is marked for the alert worker to work out again.
CREATE TABLE calendar_alerts (
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    resource_id   INTEGER NOT NULL REFERENCES dav_resources (id) ON DELETE CASCADE,
    alert_id      TEXT NOT NULL,
    -- The instance of a series it is for; NULL for an event that does not recur.
    recurrence_id TEXT,
    fire_at       INTEGER NOT NULL,
    -- `display` (a push) or `email`.
    action        TEXT NOT NULL,
    PRIMARY KEY (account_id, resource_id, alert_id, fire_at)
) WITHOUT ROWID;
CREATE INDEX calendar_alerts_due ON calendar_alerts (fire_at);

CREATE TABLE calendar_alerts_dirty (
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    resource_id INTEGER NOT NULL REFERENCES dav_resources (id) ON DELETE CASCADE,
    PRIMARY KEY (account_id, resource_id)
) WITHOUT ROWID;

-- The events with alarms there are already, for their owners.
INSERT OR IGNORE INTO calendar_alerts_dirty (account_id, resource_id)
SELECT c.account_id, r.id FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
WHERE c.kind = 'calendar' AND r.component = 'VEVENT' AND r.content LIKE '%BEGIN:VALARM%';
