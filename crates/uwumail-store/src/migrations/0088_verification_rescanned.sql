-- Codes the one-time re-scan after an update found in mail that had already arrived: kept, but not
-- counted toward the sender's hourly share, so a backlog does not crowd out a live code
-- (security-audit-0.30.0 R30-04).
ALTER TABLE email_verification ADD COLUMN rescanned INTEGER NOT NULL DEFAULT 0;
