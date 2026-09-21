-- The mail that was already in a fetched mailbox when it was set up.
--
-- A run only takes what arrives after the first one; everything that was there before stays at the
-- provider. Whoever wants it here asks for it once, and from then on the next runs work through it
-- a portion at a time next to the new mail, until the mailbox holds nothing this server has not
-- seen.
--
-- When it was asked for. NULL means nothing is waiting. A new request while one is still running
-- starts it over, which is harmless: what is already here is recognised and not brought twice.
ALTER TABLE fetch_accounts ADD COLUMN backlog_at INTEGER;

-- How far each folder has got with it. backlog_at says which request the folder is working on, so a
-- later request starts the folder over; backlog_until is the last UID that belongs to it -- the mail
-- after that is new mail and comes the usual way -- and is NULL once the folder is through.
ALTER TABLE fetch_state ADD COLUMN backlog_at INTEGER;
ALTER TABLE fetch_state ADD COLUMN backlog_next INTEGER NOT NULL DEFAULT 1;
ALTER TABLE fetch_state ADD COLUMN backlog_until INTEGER;

-- Whether a message is already here is asked by its Message-ID, once for every message of a
-- backlog, in mailboxes that can hold many thousands.
CREATE INDEX emails_account_message_id ON emails (account_id, message_id);
