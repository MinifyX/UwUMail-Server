-- UIDs that left a mailbox, with the change sequence number they left at. IMAP clients that
-- resynchronize with QRESYNC ask which messages vanished since the state they last saw.
CREATE TABLE imap_vanished (
    mailbox_id INTEGER NOT NULL,
    uid        INTEGER NOT NULL,
    modseq     INTEGER NOT NULL,
    PRIMARY KEY (mailbox_id, uid)
) WITHOUT ROWID;
CREATE INDEX imap_vanished_modseq ON imap_vanished (mailbox_id, modseq);

-- Every change takes the account's next modseq before it removes a message from a mailbox, so the
-- account's current modseq is the one the removal belongs to.
CREATE TRIGGER email_mailboxes_vanished AFTER DELETE ON email_mailboxes
BEGIN
    INSERT OR REPLACE INTO imap_vanished (mailbox_id, uid, modseq)
    SELECT OLD.mailbox_id, OLD.uid, a.modseq
    FROM mailboxes m JOIN accounts a ON a.id = m.account_id
    WHERE m.id = OLD.mailbox_id;
END;

CREATE TRIGGER mailboxes_vanished_cleanup AFTER DELETE ON mailboxes
BEGIN
    DELETE FROM imap_vanished WHERE mailbox_id = OLD.id;
END;
