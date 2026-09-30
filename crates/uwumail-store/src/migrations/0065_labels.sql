-- Labels without a model (docs/labels.md, docs/jmap-assist.md "Labels").
--
-- A label may have rules (JSON: {"match", "conditions"}), a built-in detector, and may learn from
-- senders and with its classifier. non_ai_labels switches all of that on or off per person; it is
-- on by default, unlike the model's labels (auto_labels).
ALTER TABLE assist_labels ADD COLUMN rules TEXT;
ALTER TABLE assist_labels ADD COLUMN detector TEXT;
ALTER TABLE assist_labels ADD COLUMN learn_senders INTEGER NOT NULL DEFAULT 1;
ALTER TABLE assist_labels ADD COLUMN classifier INTEGER NOT NULL DEFAULT 1;
ALTER TABLE assist_prefs ADD COLUMN non_ai_labels INTEGER NOT NULL DEFAULT 1;

-- Who put a label on: 'ai', 'rule', 'sender', 'detector' or 'classifier'; code and params (JSON)
-- say why in a form clients translate. Entries from before were all the model's.
ALTER TABLE assist_label_log ADD COLUMN source TEXT NOT NULL DEFAULT 'ai';
ALTER TABLE assist_label_log ADD COLUMN code TEXT NOT NULL DEFAULT 'ai';
ALTER TABLE assist_label_log ADD COLUMN params TEXT NOT NULL DEFAULT '{}';

-- How often the person gave mail from an address a label by hand; taking it off by hand forgets
-- the address for the label.
CREATE TABLE label_senders (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    label_id   INTEGER NOT NULL REFERENCES assist_labels (id) ON DELETE CASCADE,
    address    TEXT NOT NULL,
    count      INTEGER NOT NULL,
    PRIMARY KEY (label_id, address)
) WITHOUT ROWID;
CREATE INDEX label_senders_address ON label_senders (account_id, address);

-- The classifier's examples: mails the person labeled or unlabeled by hand, and ordinary mail
-- learned beside them, each with its tokens (JSON array of 64-bit hashes, never words). An example
-- outlives its mail: what was learned stays.
CREATE TABLE label_examples (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email_id   INTEGER NOT NULL,
    tokens     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (account_id, email_id)
);
-- The examples that have a label; all others are examples without it.
CREATE TABLE label_example_labels (
    label_id   INTEGER NOT NULL REFERENCES assist_labels (id) ON DELETE CASCADE,
    example_id INTEGER NOT NULL REFERENCES label_examples (id) ON DELETE CASCADE,
    PRIMARY KEY (label_id, example_id)
) WITHOUT ROWID;
CREATE INDEX label_example_labels_example ON label_example_labels (example_id);
-- In how many of a person's examples a token occurs, and in how many of those with a label.
CREATE TABLE label_tokens (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    token      INTEGER NOT NULL,
    examples   INTEGER NOT NULL,
    PRIMARY KEY (account_id, token)
) WITHOUT ROWID;
CREATE TABLE label_positive_tokens (
    label_id INTEGER NOT NULL REFERENCES assist_labels (id) ON DELETE CASCADE,
    token    INTEGER NOT NULL,
    examples INTEGER NOT NULL,
    PRIMARY KEY (label_id, token)
) WITHOUT ROWID;

-- Hand-labelings waiting to be learned: learning reads the mail, which a change to its keywords
-- does not wait for.
CREATE TABLE label_training (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email_id   INTEGER NOT NULL,
    label_id   INTEGER NOT NULL,
    positive   INTEGER NOT NULL,
    queued_at  INTEGER NOT NULL
);

-- A label's mail across folders: counts and `hasKeyword` without a folder.
CREATE INDEX email_keywords_keyword ON email_keywords (keyword, email_id);

-- AI for mail of other accounts (docs/jmap-assist.md "Foreign mail"): the admin's switch is off
-- until turned on; the server providers there are may serve it then, as they serve every feature.
UPDATE assist_providers SET features = json_insert(features, '$[#]', 'foreignMail')
WHERE account_id IS NULL AND NOT EXISTS (SELECT 1 FROM json_each(features) WHERE value = 'foreignMail');
