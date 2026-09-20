-- The webmail: whether this account may open its mailbox in the browser.
--
-- Deliberately its own column and not a sixth protocol switch. The protocol switches decide what
-- other mail programs may do with this account's password; the webmail is part of the server
-- itself and is reached with the portal's session. Taking away someone's mail apps should not
-- quietly take away the browser too, and switching the webmail off should not stop their phone.
--
-- Everyone who exists today keeps it, and a service account never gets in anyway: it has no
-- portal login at all.
ALTER TABLE accounts ADD COLUMN webmail_enabled INTEGER NOT NULL DEFAULT 1;
