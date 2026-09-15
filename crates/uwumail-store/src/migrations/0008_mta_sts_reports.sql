-- MTA-STS for our domains, the policies of other domains, and the reports other servers send.

-- NULL: MTA-STS is off. Otherwise 'testing' or 'enforce'.
ALTER TABLE domains ADD COLUMN mta_sts_mode TEXT;
-- JSON array of the MX names the policy lists.
ALTER TABLE domains ADD COLUMN mta_sts_mx TEXT;
ALTER TABLE domains ADD COLUMN mta_sts_changed_at INTEGER;

-- Policies of the domains we deliver to (RFC 8461), kept until they expire.
CREATE TABLE mta_sts_policies (
    domain     TEXT PRIMARY KEY,
    policy_id  TEXT NOT NULL,
    mode       TEXT NOT NULL,
    mx         TEXT NOT NULL,  -- JSON array of MX patterns
    max_age    INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL
);

-- TLS reports (RFC 8460) for our domains.
CREATE TABLE tls_reports (
    id            INTEGER PRIMARY KEY,
    domain_id     INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    organization  TEXT NOT NULL,
    report_id     TEXT NOT NULL,
    begin_at      INTEGER NOT NULL,
    end_at        INTEGER NOT NULL,
    received_at   INTEGER NOT NULL,
    authenticated INTEGER NOT NULL,  -- the report mail passed DMARC
    successful    INTEGER NOT NULL,
    failed        INTEGER NOT NULL,
    UNIQUE (domain_id, organization, report_id)
);
CREATE INDEX tls_reports_domain ON tls_reports (domain_id, end_at);

CREATE TABLE tls_report_failures (
    report_id   INTEGER NOT NULL REFERENCES tls_reports (id) ON DELETE CASCADE,
    policy_type TEXT NOT NULL,
    result_type TEXT NOT NULL,
    mx_host     TEXT NOT NULL,
    sending_ip  TEXT NOT NULL,
    sessions    INTEGER NOT NULL
);
CREATE INDEX tls_report_failures_report ON tls_report_failures (report_id);

-- DMARC aggregate reports (RFC 7489) for our domains.
CREATE TABLE dmarc_reports (
    id            INTEGER PRIMARY KEY,
    domain_id     INTEGER NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    organization  TEXT NOT NULL,
    report_id     TEXT NOT NULL,
    begin_at      INTEGER NOT NULL,
    end_at        INTEGER NOT NULL,
    received_at   INTEGER NOT NULL,
    authenticated INTEGER NOT NULL,
    policy        TEXT NOT NULL,  -- p= as the reporter saw it
    messages      INTEGER NOT NULL,
    passed        INTEGER NOT NULL,
    UNIQUE (domain_id, organization, report_id)
);
CREATE INDEX dmarc_reports_domain ON dmarc_reports (domain_id, end_at);

CREATE TABLE dmarc_report_rows (
    report_id    INTEGER NOT NULL REFERENCES dmarc_reports (id) ON DELETE CASCADE,
    source_ip    TEXT NOT NULL,
    messages     INTEGER NOT NULL,
    dkim_aligned INTEGER NOT NULL,
    spf_aligned  INTEGER NOT NULL,
    disposition  TEXT NOT NULL,
    header_from  TEXT NOT NULL
);
CREATE INDEX dmarc_report_rows_report ON dmarc_report_rows (report_id);
