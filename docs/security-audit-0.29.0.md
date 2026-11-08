# Security audit — release 0.29.0

Scope: everything new between `main` and `release/0.29.0` (`git diff origin/main...HEAD`,
lockfiles left out): sender proof for fetched addresses, resending refused mail, JMAP and web
second-factor counting, the zip/TNEF parser limits, CalDAV/CardDAV multiget and quota, IMAP
sequence sets and QRESYNC VANISHED, change pruning (migration 0087, `prune_changes`), backup and
restore locking with the database swap and rollback, the `update.sh` rollback, and migrations
0085–0087 on an existing 0.28.1 database. Read only; the fixes below were made afterwards on the
branch, tested, and checked with an upgrade from 0.28.1 on the same volume.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 0 |
| Medium | 1 |
| Low | 6 |
| Info | 4 |

The new trust boundaries hold. The sender-proof code is random, is never sent to the client,
counts only once (conditional update in a write transaction) and is the only thing that makes a
fetched address usable as a sender. Only an admin can resend refused mail, and only for an enabled
account that may send and still owns the sender address. Second-factor tries are counted before
the check. The parser limits apply before any allocation that depends on the input. The
migrations are additive and safe on a 0.28.1 database.

The one Medium finding (R29-01) was the proof mail itself: it goes out from postmaster with no
account attached, so no outbound limit or suspension applied to it. All findings were fixed before
the release.

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| R29-01 | Medium | `crates/uwumail-store/src/fetch.rs` `fetch_sender_proof_due`; `crates/uwumail-smtp/src/sender_proof.rs` | Proof mails are queued without an account and with a null sender, signed as postmaster, outside the hourly budget, suspensions, internal-only and SMTP checks. The only limit was one mail per address per day, so an account adding and deleting fetch rows could have many unsolicited mails sent from postmaster. | At most 10 proof mails per account per day (counter in the settings, kept when rows are deleted); none for accounts that are deleted, disabled, suspended, internal-only or without SMTP. Test `proof_codes_stay_within_what_the_account_may_send`. | fixed |
| R29-02 | Low | `crates/uwumail-store/src/fetch.rs`, settings `sender-proof:<address>` | The per-address marks were never removed. | Marks older than a day are deleted whenever a proof is mailed. | fixed |
| R29-03 | Low | `crates/uwumail-server/src/restore.rs` `move_into`, `swap` | Rollback put back only the database; other replaced files (certificates, DKIM keys) stayed, and `remove_file` before `rename` could lose a file. | Replaced files are moved aside, put back in reverse order on failure, and deleted only after every move worked. Tests for both outcomes. | fixed |
| R29-04 | Low | `crates/uwumail-store/src/objects.rs` `prune_changes` | The first real prune deleted all accounts' old history in one write transaction. | One write per account to raise the floor first, then deletes in batches of 5000. | fixed |
| R29-05 | Low | `crates/uwumail-store/src/imap.rs` `imap_vanished` | Below the floor every missing UID was built one by one (O(uid_next)). | Returns UID ranges; the IMAP side intersects and prints them as a sequence set. | fixed |
| R29-06 | Low | `crates/uwumail-web/src/routes/auth.rs` `passkey_login` | A database error during the passkey check counted as a wrong try. | The passkey lookup runs before the check; a store error gives the try back and answers 500. Test `a_store_error_at_passkey_login_takes_no_try`. | fixed |
| R29-07 | Low | `crates/uwumail-smtp/src/spam/attachments.rs` | A zip with more than 10,000 entries scored as an archive holding a program (+3.0). | Its own rule `ARCHIVE_UNCHECKED` (+0.5). | fixed |
| R29-08 | Info | `update.sh` | A long migration inside the 2-minute health wait could cause a rollback the old binary cannot start from. | Waits up to 15 minutes while the server is still starting; never rolls back once a migration was applied, but says so and exits 1. | fixed |
| R29-09 | Info | migration 0085 | Existing send-enabled fetch rows start unproven; a proof sorted into an unfetched Junk folder never verifies. | Upgrade notes in the CHANGELOG say so and what to do. | fixed |
| R29-10 | Info | `crates/uwumail-backup/src/service.rs`, `restore.rs` `take_over` | The backup lock lived in memory, so a CLI `backup run` in another process did not see a running restore; a crashed run's `running_at` blocked backups for hours. | An OS file lock (`backup.lock` in the data directory, left out of snapshots) shared by backups, restores and the swap on start; the stale `running_at` no longer blocks a run. | fixed |
| R29-11 | Info | `crates/uwumail-store/src/queue.rs`, `crates/uwumail-web/src/routes/queue.rs` | The portal's queue list loaded the whole queue before keeping 500. | `oldest_queue_entries(limit)` limits in SQL; health and Microsoft counts still read all. | fixed |
