-- Drafts of JMAP Calendars (`isDraft`, docs/jmap-calendars.md): an event nobody is told about
-- yet. CalDAV has no word for it; its clients see the event as it is, and no scheduling message
-- goes out for it until it stops being a draft.
ALTER TABLE dav_resources ADD COLUMN draft INTEGER NOT NULL DEFAULT 0;
