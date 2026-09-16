-- The Bayes filter: how often each token turned up in spam and in wanted mail, for the whole
-- server (account_id 0) and for each person who marked mail themselves. Tokens are keyed hashes,
-- so no word from anyone's mail is stored readable.
CREATE TABLE bayes_tokens (
    account_id INTEGER NOT NULL,
    token      INTEGER NOT NULL,
    spam       INTEGER NOT NULL DEFAULT 0,
    ham        INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, token)
) WITHOUT ROWID;
CREATE INDEX bayes_tokens_updated ON bayes_tokens (updated_at);

-- How many messages each scope learned as spam and as wanted mail.
CREATE TABLE bayes_totals (
    account_id INTEGER PRIMARY KEY,
    spam       INTEGER NOT NULL DEFAULT 0,
    ham        INTEGER NOT NULL DEFAULT 0
);

-- What a stored message was learned as in a scope, so changing one's mind unlearns it first.
CREATE TABLE bayes_learned (
    blob_hash  TEXT NOT NULL,
    account_id INTEGER NOT NULL,
    spam       INTEGER NOT NULL,
    learned_at INTEGER NOT NULL,
    PRIMARY KEY (blob_hash, account_id)
) WITHOUT ROWID;
CREATE INDEX bayes_learned_at ON bayes_learned (learned_at);

-- Messages waiting to be learned; reading and splitting a message happens outside the transaction
-- that marked it.
CREATE TABLE bayes_queue (
    id         INTEGER PRIMARY KEY,
    blob_hash  TEXT NOT NULL,
    account_id INTEGER NOT NULL,
    spam       INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
