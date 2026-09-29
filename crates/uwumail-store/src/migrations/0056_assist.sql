-- The AI assistant (docs/llm.md, docs/jmap-assist.md).
--
-- A provider is a way to reach a language model: one the admin set up for the server
-- (account_id NULL) or one a person added with their own key. The key (or, for a ChatGPT login,
-- the tokens as JSON) is sealed like the passwords of fetched mailboxes; key_hint keeps its last
-- four characters to recognise it by. For server providers, access decides who may use them
-- ('everyone', the domains in access_list, or the logins in access_list), features which features
-- (a JSON array), and the quota how much per person and day (NULL: no limit).
CREATE TABLE assist_providers (
    id                INTEGER PRIMARY KEY,
    account_id        INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    kind              TEXT NOT NULL,
    base_url          TEXT,
    secret            BLOB,
    key_hint          TEXT,
    model             TEXT,
    fast_model        TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    access            TEXT NOT NULL DEFAULT 'everyone' CHECK (access IN ('everyone', 'domains', 'people')),
    access_list       TEXT NOT NULL DEFAULT '[]',
    features          TEXT NOT NULL DEFAULT '["compose","summarize","spamCheck","extractEvents","autoLabels"]',
    requests_per_day  INTEGER,
    tokens_per_day    INTEGER,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
CREATE INDEX assist_providers_account ON assist_providers (account_id);

-- A person's choices: the default provider and model, one per feature (JSON), and whether labels
-- are put on incoming mail. modseq is the JMAP state of their assist objects.
CREATE TABLE assist_prefs (
    account_id  INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    choices     TEXT NOT NULL DEFAULT '{}',
    auto_labels INTEGER NOT NULL DEFAULT 0,
    modseq      INTEGER NOT NULL DEFAULT 0
);

-- A person's labels. keyword is what goes on the email, set once and never changed.
CREATE TABLE assist_labels (
    id          INTEGER PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    keyword     TEXT NOT NULL,
    color       TEXT,
    created_at  INTEGER NOT NULL,
    UNIQUE (account_id, keyword)
);

-- Delivered mail waiting for its labels. Dropped after a few tries or a day.
CREATE TABLE assist_label_queue (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email_id   INTEGER NOT NULL,
    queued_at  INTEGER NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    next_at    INTEGER NOT NULL,
    UNIQUE (account_id, email_id)
);
CREATE INDEX assist_label_queue_due ON assist_label_queue (next_at);

-- Which label the model put on which email and why, for the reader's "why" and undo.
CREATE TABLE assist_label_log (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email_id   INTEGER NOT NULL,
    label_id   INTEGER NOT NULL REFERENCES assist_labels (id) ON DELETE CASCADE,
    reason     TEXT NOT NULL DEFAULT '',
    provider   TEXT NOT NULL DEFAULT '',
    model      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    undone_at  INTEGER
);
CREATE INDEX assist_label_log_email ON assist_label_log (account_id, email_id);

-- What each person used, per day (UTC, 'YYYY-MM-DD'), provider and feature. provider_id is kept
-- after the provider is gone, so the numbers stay.
CREATE TABLE assist_usage (
    account_id    INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    provider_id   INTEGER NOT NULL,
    day           TEXT NOT NULL,
    feature       TEXT NOT NULL,
    requests      INTEGER NOT NULL DEFAULT 0,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, provider_id, day, feature)
);
CREATE INDEX assist_usage_day ON assist_usage (day);
