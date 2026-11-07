# Security audit — sending refused mail again (0.28.1)

Scope: `git diff main -- . ':!Cargo.lock'` on `release/0.28.1`: the kept copy of refused mail
(`uwumail-store/src/migrations/0084_refused_resend.sql`, `queue.rs` `note_refused`,
`refused_deliveries`, `resend_refused`, `refusal_cause`), the purge in `blobs.rs`
`collect_garbage`, the route `POST /api/admin/queue/refused/{id}/resend`
(`uwumail-web/src/routes/queue.rs`, `lib.rs`) and the button in `web/src/features/queue/RefusedCard.tsx`.
Read only; nothing was built or run.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 0 |
| Medium | 2 |
| Low | 3 |
| Info | 1 |

The reference counting holds: the new triggers count up on insert and down on every delete
(`clear_refused_deliveries`, the 5000-row cap and the 30-day cut in `note_refused`, the purge in
`collect_garbage`, `resend_refused`), the purge runs in the same transaction before the blob
delete, and `resend_refused` inserts the new queue message (its own `queue_blob_ref` trigger)
before it deletes the refused row, so the count never drops to zero in between. `blob_hash` has a
foreign key to `blobs` like `queue_messages.blob_hash`. The route uses the `Admin` extractor, which
checks the CSRF header on every non-GET request, and writes to the audit log.

Both medium findings (forwards keeping a copy, sending again ignoring the sender's state) and
the low findings RR-03 and RR-04 were fixed on the branch before the release; RR-05 and RR-06
are accepted.

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| RR-01 | Medium | `store/src/queue.rs` `note_refused` | A copy is kept when the queue message has an `account_id` and a non-empty return path. Forwards (`smtp/src/forward.rs`) are queued with the forwarder's `account_id` and an SRS return path, so they keep a copy too, contrary to the changelog and the migration comment. Their content comes from outside: anyone can send large mails to a forwarding address whose target refuses them and so pin up to 5000 × 50 MB for 30 days (before, the file went with the queue entry), and an admin's "Fixed" sends that outside content out again. | Keep a copy only for the account's own mail, by the same rule as OA-02: the return path must belong to `m.account_id` (`address_owner`, or the same join over `addresses`/`domains` inside the `INSERT … SELECT`). Add a test that a forward keeps no copy. | fixed: copy only when the return path is an address of a live account of the sender; test with an SRS forward |
| RR-02 | Medium | `store/src/queue.rs` `resend_refused` | Sending again ignores the sender's state now. The new queue entries skip the submission checks, so they go out even when the account is suspended (0.28.0), in the trash (`trash_account` cancels held mail for that reason) or deleted (`account_id` turned NULL by `ON DELETE SET NULL`, so `held_inside` and `sender_route` no longer apply to it at all). Because one click takes every refusal with the same cause, mail from other accounts goes too, and a Microsoft block list such as S3150 is exactly what a spamming account causes: fixing the legitimate entry sends the spammer's refused mail again. Also, a deleted person's mail stays on the server for up to 30 days. | In `resend_refused`, leave entries whose account is NULL, in the trash or suspended right now on the list and report them separately (e.g. `skipped`); in `delete_account` delete the account's `refused_deliveries` rows before the account. | fixed: suspended, trashed or deleted senders are left on the list and counted as `skipped`; `delete_account` removes their rows; tested |
| RR-03 | Low | `store/src/queue.rs` `refusal_cause` | A `(S<digits>)` anywhere in the reply makes the cause the block list alone, without domain, whoever sent the reply. A server elsewhere chooses its own reply text, so it can put its refusals into the same group as Microsoft's. The recipients stay the ones the mail was meant for, and only mail our accounts already sent to that server is affected, so nothing reaches anyone unintended. | Use the block list only for replies from Microsoft (`microsoft::is_microsoft_host` is known when the refusal is noted: store a flag, or check the recipient domain against Microsoft's consumer domains); otherwise fall back to code and domain. | fixed: block list grouping only for hotmail./outlook./live./msn. recipient domains; tested |
| RR-04 | Low | `store/src/queue.rs` `note_refused` | Even with RR-01 fixed, an own account can keep 5000 different messages up to the message size limit for 30 days by sending to addresses that refuse them; this is outside its quota. Bounded by the outbound hourly budget and the row cap. | Keep no copy above a size (e.g. 10 MB), or stop keeping copies once the kept bytes pass a limit. | fixed: no copy above 10 MB (`REFUSED_COPY_MAX_BYTES`) |
| RR-05 | Low | `web/.../RefusedCard.tsx`, `web/src/routes/queue.rs` `resend_refused` | One click sends a whole group again, possibly hundreds of mails from several accounts, with no confirmation and no count shown first. The audit entry records only the id and the totals, not the cause or which entries went out. | Ask for confirmation with the number of entries that share the cause; put the cause into the audit details. | accepted: the admin asked for one click; the toasts give the counts afterwards |
| RR-06 | Info | `store/src/queue.rs` `resend_refused` | An entry that gave up after timeouts may have arrived after all, so sending it again can deliver twice; sending again what failed with 5.1.1 counts bounce strikes again (OA-02). Revoked identities are not checked: the kept copy carries its original DKIM signature. | — | accepted: an admin decision, bounded by the list. |

Checked and found fine: `last_insert_rowid` (read only after the `INSERT … SELECT` reported one
row; the `queue_blob_ref` trigger only updates, and SQLite restores the value after a trigger),
`refusal_cause` on outside text (no panics: `inside[1..]` only after an ASCII `S`, error text
capped at 500 characters), two clicks at once (the write transaction serialises them; the second
gets 404), old entries from before the migration (NULL `blob_hash`, the triggers skip them, they
are only dropped), mail from accounts that may only send internally (still refused at delivery by
`held_inside`), the API (only a `canResend` flag, no hash or content; admin only), and the list
query and the purge (both use the same 30-day cut).
