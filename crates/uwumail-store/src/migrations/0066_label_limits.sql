-- Limits for what labels learn (security audit 0.21.0): a hand-labeling waits at most once per
-- email and label (the last change wins), so putting a label on and off again and again queues
-- nothing more.
DELETE FROM label_training WHERE id NOT IN (
    SELECT MAX(id) FROM label_training GROUP BY account_id, email_id, label_id
);
CREATE UNIQUE INDEX label_training_email ON label_training (account_id, email_id, label_id);
