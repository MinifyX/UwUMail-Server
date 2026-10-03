-- How often a move was running when the server started without having stopped cleanly. A message
-- that crashes the server every time would otherwise start the crash again after each restart; at
-- the third time the move is paused instead (security review 0.22 M-1). A turn that ends sets it
-- back to 0.
ALTER TABLE move_mailboxes ADD COLUMN interrupted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE migration_jobs ADD COLUMN interrupted INTEGER NOT NULL DEFAULT 0;
