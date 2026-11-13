-- The UNIQUE (mailbox_id, uid) constraint of email_mailboxes already keeps this exact index.
DROP INDEX IF EXISTS email_mailboxes_mailbox;
