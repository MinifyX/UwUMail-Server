-- Answering from a fetched address needs proof that the address is its owner's.
--
-- A successful fetch only shows that the login opens *a* mailbox; the address the row names is
-- whatever was typed in. So before an account may send as it, a message with a secret code goes
-- to the address itself, and only when that message turns up in the fetched mailbox is the address
-- taken as the account's (security audit 2026-11 M-01). Rows that could send before start out
-- unproven too: the fetcher mails them their code by itself, so nobody has to do anything.
ALTER TABLE fetch_accounts ADD COLUMN sender_verified_at INTEGER;
-- The code that is out, and when it was last mailed (at most once a day per row).
ALTER TABLE fetch_accounts ADD COLUMN sender_proof TEXT;
ALTER TABLE fetch_accounts ADD COLUMN sender_proof_sent_at INTEGER;
