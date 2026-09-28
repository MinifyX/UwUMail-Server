-- What people someone shares folders with see of the owner's changes (security audit 0.16.0,
-- PROTOCOLS-L2).
--
-- A shared account's JMAP state was the owner's change counter: it moved, and push said so, with
-- everything the owner did in folders nobody else sees. The state a sharee sees is now the last
-- change to a mailbox shared with them or to what it holds (`mailboxes.updated_modseq`, which every
-- change to a mailbox or to an email in it moves on from now), or to the sharing itself.
--
-- `sharing_modseq` is the owner's change at which who sees what last changed: a folder shared or
-- unshared, rights changed, members of a shared mailbox changed, a shared folder deleted. Changes
-- across it cannot be calculated for a sharee; the client loads the account again.
ALTER TABLE accounts ADD COLUMN sharing_modseq INTEGER NOT NULL DEFAULT 0;

-- Until now `updated_modseq` did not move with every change to what a mailbox holds. Starting from
-- the account's counter keeps every state and IMAP HIGHESTMODSEQ from going backwards.
UPDATE mailboxes SET updated_modseq = max(updated_modseq, (SELECT modseq FROM accounts WHERE id = mailboxes.account_id));
