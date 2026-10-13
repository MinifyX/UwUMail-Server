-- A network that more than one account logged in from is shared: an office, a university, one
-- address for many of a carrier's customers. It is remembered for a year instead of a month, and
-- one bad login from it is counted, not blocked (limiter.rs). `login` is the one the network is
-- known by, so the second one can be told from the first after a restart too.
ALTER TABLE known_networks ADD COLUMN login TEXT NOT NULL DEFAULT '';
ALTER TABLE known_networks ADD COLUMN shared INTEGER NOT NULL DEFAULT 0;
