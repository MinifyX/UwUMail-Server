-- Base label definitions that change with a later set (docs/labels.md, "Base labels"): the
-- definition as the server wrote it and its language. A stored description still equal to what
-- was written is the server's and gets the new wording; one that differs is kept.
ALTER TABLE assist_labels ADD COLUMN base_written TEXT;
ALTER TABLE assist_labels ADD COLUMN base_language TEXT;
UPDATE assist_labels SET base_written = description WHERE base IS NOT NULL;
