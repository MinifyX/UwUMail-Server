-- JMAP in 0.12: sending held back for the undo window or for later, and the undo window as a
-- portal preference.

-- A submission that waits (undo_status 'pending') keeps the message it will send: the blob of the
-- email as it was submitted, so editing or deleting the draft afterwards changes nothing. It stays
-- set while the message is being handed over (undo_status 'final') and is cleared once that is
-- done, so a submission that was claimed but not finished when the server stopped is sent after
-- the restart.
ALTER TABLE email_submissions ADD COLUMN held_blob TEXT REFERENCES blobs (hash);
-- Why a held message could not be sent when its time came, for deliveryStatus.
ALTER TABLE email_submissions ADD COLUMN release_error TEXT;
CREATE INDEX email_submissions_held ON email_submissions (send_at) WHERE held_blob IS NOT NULL;

CREATE TRIGGER email_submissions_held_ref AFTER INSERT ON email_submissions
WHEN NEW.held_blob IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.held_blob;
END;

CREATE TRIGGER email_submissions_held_change AFTER UPDATE OF held_blob ON email_submissions
WHEN OLD.held_blob IS NOT NEW.held_blob
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.held_blob;
    UPDATE blobs SET refs = refs + 1 WHERE hash = NEW.held_blob;
END;

CREATE TRIGGER email_submissions_held_unref AFTER DELETE ON email_submissions
WHEN OLD.held_blob IS NOT NULL
BEGIN
    UPDATE blobs SET refs = refs - 1 WHERE hash = OLD.held_blob;
END;

-- The undo window (UserSettings `undoSendSeconds`) now lives in the portal preferences as
-- `mailUndoSend`, like the other shared settings, so the server can apply it to every submission.
UPDATE accounts
SET preferences = json_set(
    CASE WHEN json_valid(preferences) THEN preferences ELSE '{}' END,
    '$.mailUndoSend',
    (SELECT CAST(s.value AS TEXT) FROM user_settings s
     WHERE s.account_id = accounts.id AND s.key = 'undoSendSeconds')
)
WHERE EXISTS (
    SELECT 1 FROM user_settings s
    WHERE s.account_id = accounts.id AND s.key = 'undoSendSeconds' AND s.value IN ('0', '5', '10', '20', '30')
);
DELETE FROM user_settings WHERE key = 'undoSendSeconds';
