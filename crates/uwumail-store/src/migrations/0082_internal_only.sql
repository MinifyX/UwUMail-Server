-- An account that may only send to addresses on this server (docs/jmap-sending.md): every
-- recipient on a domain outside the domains table is refused, forwarding and vacation replies
-- leave nothing outside either. Off for everyone.
ALTER TABLE accounts ADD COLUMN internal_only INTEGER NOT NULL DEFAULT 0;
