-- BIMI per domain (docs/bimi.md): the logo as SVG Tiny PS, already cleaned, and an optional mark
-- certificate (VMC or CMC, PEM), both served at a fixed HTTPS address while BIMI is on.
CREATE TABLE domain_bimi (
    domain_id              INTEGER PRIMARY KEY REFERENCES domains (id) ON DELETE CASCADE,
    enabled                INTEGER NOT NULL DEFAULT 0,
    title                  TEXT NOT NULL DEFAULT '',
    svg                    TEXT,
    svg_updated_at         INTEGER,
    certificate            TEXT,
    certificate_updated_at INTEGER
);
