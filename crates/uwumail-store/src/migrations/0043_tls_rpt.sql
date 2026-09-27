-- TLS reports this server sends to other domains (RFC 8460, docs/tls-reports.md): every delivery
-- session to another domain's MX counts once, per UTC day and per policy the session followed, and
-- a daily job turns a finished day into one report per domain that asks for them.

-- `day` is the UTC day as days since 1970-01-01. `policy_type` is "sts", "tlsa" or
-- "no-policy-found"; `policy_string` and `mx_host` are JSON arrays of strings as the report wants
-- them. An empty `result_type` is a successful session; failures keep where they went wrong.
CREATE TABLE tls_rpt_sessions (
    day                   INTEGER NOT NULL,
    policy_domain         TEXT NOT NULL,
    policy_type           TEXT NOT NULL,
    policy_string         TEXT NOT NULL DEFAULT '[]',
    mx_host               TEXT NOT NULL DEFAULT '[]',
    result_type           TEXT NOT NULL DEFAULT '',
    receiving_mx_hostname TEXT NOT NULL DEFAULT '',
    receiving_ip          TEXT NOT NULL DEFAULT '',
    sending_ip            TEXT NOT NULL DEFAULT '',
    count                 INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, policy_domain, policy_type, policy_string, mx_host, result_type,
                 receiving_mx_hostname, receiving_ip, sending_ip)
);

-- One row per day and domain once its report was dealt with: "sent", "failed" (tried again the
-- next day, once), "none" (the domain asks for no reports) or "skipped".
CREATE TABLE tls_rpt_sent (
    day           INTEGER NOT NULL,
    policy_domain TEXT NOT NULL,
    report_id     TEXT NOT NULL,
    status        TEXT NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 1,
    -- JSON array of the addresses (mailto: and https:) it went to.
    destinations  TEXT NOT NULL DEFAULT '[]',
    error         TEXT NOT NULL DEFAULT '',
    successful    INTEGER NOT NULL DEFAULT 0,
    failed        INTEGER NOT NULL DEFAULT 0,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (day, policy_domain)
);
