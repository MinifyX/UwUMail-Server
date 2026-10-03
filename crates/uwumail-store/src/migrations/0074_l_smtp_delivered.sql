-- Whether this server's SMTP delivery stored the message, so the block of headers it wrote on top
-- (Received, Authentication-Results, X-Spam-*) is its own. Mail stored any other way (IMAP APPEND,
-- JMAP Email/import, an .eml) can carry a forged copy of that block, and its verdicts are not read
-- (security review 0.22 R2-L2).
ALTER TABLE emails ADD COLUMN smtp_delivered INTEGER NOT NULL DEFAULT 0;
-- Mail stored before this column existed cannot be told apart; it is read as before.
UPDATE emails SET smtp_delivered = 1;
