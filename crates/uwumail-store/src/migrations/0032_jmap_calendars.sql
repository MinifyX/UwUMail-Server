-- JMAP Calendars on the CalDAV tables (docs/jmap-calendars.md).
--
-- Whether the webmail and the apps show a calendar's events. CalDAV has no word for it.
ALTER TABLE dav_collections ADD COLUMN is_visible INTEGER NOT NULL DEFAULT 1;
-- The calendar new events go into when a client has no better idea: at most one per account.
ALTER TABLE dav_collections ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;

UPDATE dav_collections SET is_default = 1
WHERE kind = 'calendar'
  AND id = (SELECT first.id FROM dav_collections first
            WHERE first.account_id = dav_collections.account_id AND first.kind = 'calendar'
            ORDER BY first.sort_order, first.id LIMIT 1);
