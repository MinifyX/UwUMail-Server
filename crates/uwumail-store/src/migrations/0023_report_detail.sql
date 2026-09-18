-- What the reports actually say, beside the totals.
--
-- Until now a DMARC row kept six numbers, and DKIM and SPF were folded into one "passed". That
-- answers "how much got through" but never "what is failing", which is the question a report is
-- sent to answer. The authentication results were in the report all along; they were dropped while
-- parsing. The same for TLS: the reason a session failed was thrown away with it.
--
-- Old rows keep NULL here. The portal shows what it has and says nothing about the rest.

-- The domain the report is about, which for a subdomain is not the domain we file it under.
ALTER TABLE dmarc_reports ADD COLUMN reported_domain TEXT;
-- The policy the reporter saw, beyond p=: sp=, adkim=, aspf=, pct=.
ALTER TABLE dmarc_reports ADD COLUMN subdomain_policy TEXT;
ALTER TABLE dmarc_reports ADD COLUMN alignment TEXT;  -- e.g. "adkim=s aspf=r"
-- Where the report came from, for asking back.
ALTER TABLE dmarc_reports ADD COLUMN contact TEXT;

-- The DKIM signature the reporter checked, and what came of it.
ALTER TABLE dmarc_report_rows ADD COLUMN dkim_domain TEXT;
ALTER TABLE dmarc_report_rows ADD COLUMN dkim_selector TEXT;
ALTER TABLE dmarc_report_rows ADD COLUMN dkim_result TEXT;  -- pass, fail, none, policy, neutral, temperror, permerror
-- The SPF check of the envelope sender.
ALTER TABLE dmarc_report_rows ADD COLUMN spf_domain TEXT;
ALTER TABLE dmarc_report_rows ADD COLUMN spf_result TEXT;
-- Why the reporter did not apply the policy it found, e.g. a forwarder it knows.
ALTER TABLE dmarc_report_rows ADD COLUMN override_reason TEXT;
ALTER TABLE dmarc_report_rows ADD COLUMN envelope_from TEXT;
ALTER TABLE dmarc_report_rows ADD COLUMN envelope_to TEXT;

-- The detail of a failed TLS session.
ALTER TABLE tls_report_failures ADD COLUMN failure_code TEXT;
ALTER TABLE tls_report_failures ADD COLUMN receiving_ip TEXT;
ALTER TABLE tls_report_failures ADD COLUMN helo TEXT;
ALTER TABLE tls_report_failures ADD COLUMN detail TEXT;

-- The policy string the sender applied, so a mismatch with ours is visible.
ALTER TABLE tls_reports ADD COLUMN policy_domain TEXT;
ALTER TABLE tls_reports ADD COLUMN policy_string TEXT;
ALTER TABLE tls_reports ADD COLUMN contact TEXT;

-- Listing reports newest first, which is how the portal reads them.
CREATE INDEX dmarc_reports_recent ON dmarc_reports (domain_id, id DESC);
CREATE INDEX tls_reports_recent ON tls_reports (domain_id, id DESC);
