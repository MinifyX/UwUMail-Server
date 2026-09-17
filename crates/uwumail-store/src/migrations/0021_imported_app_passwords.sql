-- App passwords taken over from another server keep that server's hash (e.g. bcrypt), since their
-- secret is not known here. Their secret_hash is random and never matches.
ALTER TABLE app_passwords ADD COLUMN imported_hash TEXT;
