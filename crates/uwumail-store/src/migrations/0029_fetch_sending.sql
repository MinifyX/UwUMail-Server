-- Answering from a fetched address.
--
-- A reply from a free mail address has to leave through that provider's own outgoing server. Sent
-- from here it would carry our name on the envelope while claiming theirs in the From header, and
-- their DMARC policy would take it apart at the recipient -- the very policy that makes the address
-- worth something.
--
-- So a fetched mailbox can learn where its provider accepts outgoing mail. The password is the one
-- that is already stored for fetching: providers use the same one for both, and a second one to
-- keep in sync would only be a second one to get wrong.
ALTER TABLE fetch_accounts ADD COLUMN smtp_host TEXT NOT NULL DEFAULT '';
ALTER TABLE fetch_accounts ADD COLUMN smtp_port INTEGER NOT NULL DEFAULT 587;
-- starttls (usually port 587) or tls (usually 465). Never unencrypted: this sends a password
-- across the internet, not across a machine room.
ALTER TABLE fetch_accounts ADD COLUMN smtp_security TEXT NOT NULL DEFAULT 'starttls'
    CHECK (smtp_security IN ('starttls', 'tls'));

-- Whether this address may be sent from at all. Off until someone fills in a server and asks for
-- it, so no address starts out claiming it can send when it cannot.
ALTER TABLE fetch_accounts ADD COLUMN send_enabled INTEGER NOT NULL DEFAULT 0;
