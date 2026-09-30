-- What the AI assistant costs (docs/llm.md, "Costs").
--
-- A provider's prices, in US dollars per million tokens, when the admin or the person set them
-- (NULL: taken from the price lists the server fetches), and whether the people using a server
-- provider see what it costs. Usage keeps what each day's requests cost, in US dollars at the
-- prices of the time; NULL where the price was not known (and for everything before this).
ALTER TABLE assist_providers ADD COLUMN input_price REAL;
ALTER TABLE assist_providers ADD COLUMN output_price REAL;
ALTER TABLE assist_providers ADD COLUMN show_cost INTEGER NOT NULL DEFAULT 0;
ALTER TABLE assist_usage ADD COLUMN cost_usd REAL;
