-- Whether an account holds a blob is asked on every blob download and fetched message; without an
-- index that read every message of the account.
CREATE INDEX IF NOT EXISTS emails_account_blob ON emails (account_id, blob_hash);
