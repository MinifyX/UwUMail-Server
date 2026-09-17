-- A person's own spam limits: from how many points their mail goes to Junk and from how many it is
-- refused. NULL follows the server's settings.
ALTER TABLE accounts ADD COLUMN spam_junk_score REAL;
ALTER TABLE accounts ADD COLUMN spam_reject_score REAL;
