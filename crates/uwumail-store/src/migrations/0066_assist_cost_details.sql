-- The details of what an AI request costs (docs/llm.md, "Costs").
--
-- Usage keeps, next to the tokens in and out, the tokens the model spent thinking (reasoning; not
-- part of output_tokens), the tokens of the prompt read from the provider's cache (part of
-- input_tokens), and how many calls to the model the requests took (a request the provider refused
-- for its answer format is asked once more). A provider may have a price per request set by hand, in
-- US dollars (NULL: from the price lists).
ALTER TABLE assist_usage ADD COLUMN reasoning_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assist_usage ADD COLUMN cached_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assist_usage ADD COLUMN calls INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assist_providers ADD COLUMN request_price REAL;
