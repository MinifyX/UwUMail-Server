-- What AI requests really took compared with what the server expected, so `Assist/estimate` can
-- learn from it (docs/jmap-assist.md): the last 50 requests per provider, model and feature, with
-- the prompt's and a typical answer's estimated tokens, and the tokens the provider reported. No
-- mail content, no person.
CREATE TABLE assist_calibration (
    id                INTEGER PRIMARY KEY,
    provider_id       INTEGER NOT NULL,
    model             TEXT NOT NULL,
    feature           TEXT NOT NULL,
    estimated_input   INTEGER NOT NULL,
    estimated_output  INTEGER NOT NULL,
    input_tokens      INTEGER NOT NULL,
    output_tokens     INTEGER NOT NULL,
    reasoning_tokens  INTEGER NOT NULL,
    calls             INTEGER NOT NULL,
    created_at        INTEGER NOT NULL
);
CREATE INDEX assist_calibration_key ON assist_calibration (provider_id, model, feature, id);
