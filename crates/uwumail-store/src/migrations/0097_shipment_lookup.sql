-- Asking the carrier about a parcel by its number (docs/jmap-shipments.md): when it last answered,
-- what went wrong, when it is due again and when an app last asked for it by hand.
ALTER TABLE shipments ADD COLUMN looked_up_at INTEGER;
ALTER TABLE shipments ADD COLUMN lookup_error TEXT;
ALTER TABLE shipments ADD COLUMN lookup_due_at INTEGER;
ALTER TABLE shipments ADD COLUMN refreshed_at INTEGER;
