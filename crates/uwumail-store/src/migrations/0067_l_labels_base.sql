-- Labels 0.22 (docs/labels.md): base labels, nearest neighbours, corrections for the model.
--
-- Base labels: eight fixed labels every person has (base = 'invoice', 'shipping', 'appointment',
-- 'newsletter', 'account', 'personal', 'work', 'advertising'). They are made, or a label of the same
-- name is adopted, the first time a person's labels are needed (assist_prefs.base_labels records
-- which version of the set was made). auto = 0: the label is only put on by hand.
ALTER TABLE assist_labels ADD COLUMN base TEXT;
ALTER TABLE assist_labels ADD COLUMN auto INTEGER NOT NULL DEFAULT 1;
CREATE UNIQUE INDEX assist_labels_base ON assist_labels (account_id, base) WHERE base IS NOT NULL;
ALTER TABLE assist_prefs ADD COLUMN base_labels INTEGER NOT NULL DEFAULT 0;

-- The embedding of an example (one signed byte per dimension after a 4-byte scale), by the model
-- that made it. Gone with the example, and with the email (unlike the example's tokens).
CREATE TABLE label_vectors (
    example_id INTEGER PRIMARY KEY REFERENCES label_examples (id) ON DELETE CASCADE,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    model      TEXT NOT NULL,
    vector     BLOB NOT NULL
);
CREATE INDEX label_vectors_account ON label_vectors (account_id, model);

-- A person's own corrections, shown to the model as examples: a label put on (positive) or taken
-- off by hand, with the sender's domain, the subject and the start of the text, cut short. A few
-- per label; gone with the email or the label.
CREATE TABLE label_shots (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    label_id      INTEGER NOT NULL REFERENCES assist_labels (id) ON DELETE CASCADE,
    email_id      INTEGER NOT NULL,
    positive      INTEGER NOT NULL,
    sender_domain TEXT NOT NULL,
    subject       TEXT NOT NULL,
    snippet       TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    UNIQUE (label_id, email_id)
);
CREATE INDEX label_shots_email ON label_shots (account_id, email_id);
