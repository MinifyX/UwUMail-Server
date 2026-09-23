# Security review — JMAP Calendars, Sieve and ManageSieve, 23 September 2026

The seventh pass, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md),
[security-audit-0.3.0.md](security-audit-0.3.0.md),
[security-audit-0.4.0.md](security-audit-0.4.0.md),
[security-audit-0.5.0.md](security-audit-0.5.0.md) and
[security-audit-0.5.2.md](security-audit-0.5.2.md). This one covers the code new on the
`feat/calendar-rules` branch for the planned **0.7.0** release, compared against `main`.

Done with Claude, not an independent firm. An honest sweep, not a certificate. Nothing was run
against the production server; the dynamic tests used a throwaway local server built from this
branch, with two test accounts on a test domain.

## Scope

New code only, `git diff main...HEAD`:

- **JMAP Calendars** on the existing CalDAV store: `crates/uwumail-jmap/src/jscal.rs`,
  `methods/calendar.rs`, `methods/calendar_event.rs`, `crates/uwumail-store/src/calendar.rs`,
  the `dav.rs` change log and `crates/uwumail-store/src/ical.rs`
  ([docs/jmap-calendars.md](jmap-calendars.md)).
- **Sieve at delivery**: `crates/uwumail-smtp/src/sieve.rs`, `rules.rs`, the `inbound.rs` hook
  and the `forward.rs` change ([docs/sieve.md](sieve.md)).
- **JMAP Sieve** (RFC 9661): `methods/sieve.rs`, `store/sieve.rs`, the `blob.rs` download change.
- **ManageSieve** (RFC 5804): `crates/uwumail-imap/src/managesieve.rs`.

### What held up

- **Access control across accounts.** Every JMAP calendar, event, instance, participant-identity
  and Sieve method resolves ids only within the signed-in account; a foreign id is `notFound` or
  `accountNotFound`, never another account's object. `Calendar/set onSuccessSetIsDefault`,
  `SieveScript onSuccessActivateScript/onSuccessDeactivateScript`, `calendarIds` in create and
  update, and moving an event between calendars all stay inside the account. All confirmed against
  the running server (mini vs. ami). Every SQL statement scopes by `account_id`.
- **Blobs as script source.** `SieveScript/set` and `/validate` accept a blob id only when it is
  the account's own upload or one of its scripts; a message blob, another account's upload or
  another account's script blob is `blobNotFound`. A script's own blob downloads as
  `application/sieve` with `Content-Disposition: attachment` and `nosniff`; a foreign account's
  script blob, or the same id under a foreign account path, is `404`.
- **Patch semantics.** `CalendarEvent/set` refuses patches into `method`, `uid`, `@type`, `id`,
  `isOrigin`, `baseEventId`, `recurrenceId` and the per-series properties of an instance, rejects
  overlapping pointer paths, and keeps `isDraft` false. The stored object goes through the same
  `check_calendar` a CalDAV PUT does, so JMAP cannot write iCalendar a phone could not read back.
- **Recurrence limits.** The 10 000-occurrence expansion cap and the `P400D` window hold; hostile
  time-zone ids are refused (exact IANA names only); `minDateTime`/`maxDateTime`, interval, count,
  by-part and title/size limits hold.
- **Sieve delivery.** Junk never reaches the script; `discard` stores nowhere but returns 250 and
  suppresses the vacation reply; a script failure keeps the message in the inbox; redirect follows
  the forwarding path (local or confirmed external target only, one per message, never to self, no
  loop, SRS for remote); the log carries neither script nor message. The two sieve-rs line-1
  workarounds are re-declared by the script itself, so user content cannot smuggle a capability.
- **ManageSieve.** STARTTLS before `AUTHENTICATE PLAIN`; anything buffered behind STARTTLS drops
  the connection; a SASL authzid that differs from the authcid is refused; failed logins share the
  IMAP lockout; `UNAUTHENTICATE` clears the account; the IMAP protocol switch covers ManageSieve;
  name validation refuses control characters and U+2028/2029.

## Summary

| Severity | Found | Fixed |
| --- | --- | --- |
| High | 2 | 2 |
| Medium | 5 | 5 |
| Low | 1 | 0 |

Plus one correctness bug found by the webmail end-to-end run, fixed here (`span` durations).

## Findings

### S-42 · High · One ManageSieve command could pile up literals without bound

`crates/uwumail-imap/src/managesieve.rs:344` (`read_command`)

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H` (High, 7.5)
- **Attacker & preconditions:** anyone who can reach port 4190, before logging in.
- **Impact:** a command was read as a first line plus one line per literal, each literal checked
  against the per-literal limit but nothing checking their sum. A single command of
  `NOOP {4000+}…{4000+}…` repeated held every literal in memory. Verified against the running
  server: a 40 MB command before login grew the process from 36 MB to 77 MB.
- **Fix:** a command now has at most eight lines, keeps at most 4 KiB of literals before login and a
  script plus its name after, and discards no more than 1 MiB in all before the connection ends.
- **Regression test:** `crates/uwumail-imap/tests/managesieve.rs` — a 64-literal command is closed
  before and after login, while a real `PUTSCRIPT` of a large script still works.

### S-43 · High · A runaway Sieve run held a blocking thread and stalled delivery

`crates/uwumail-smtp/src/rules.rs:143` (`plan`)

- **CVSS 3.1:** `AV:N/AC:H/PR:L/UI:N/S:C/C:N/I:N/A:H` (High, 7.6)
- **Attacker & preconditions:** anyone who can send a message to an account whose active script has
  a costly test; a person can install the script for their own account.
- **Impact:** the engine counts instructions, not the work of a single test — a `header :matches`
  over a long value is one instruction but seconds of CPU. The 10-second timeout only abandoned the
  `await`; the `spawn_blocking` thread ran on. Since the store also runs on the blocking pool, a
  stream of such messages starved the pool and stalled delivery for everyone.
- **Fix:** runs take turns in a few slots (half the cores, two to eight); a run that outlives its
  timeout keeps its slot until it is really done, and an account whose run is still going on skips
  its script meanwhile. The message is kept in the inbox in every such case.
- **Regression test:** `rules.rs` unit test — a runaway holds its slot, its account waits, others
  time out, and everyone runs again once it finishes.

### S-44 · Medium · A Sieve redirect that reached nobody dropped the message

`crates/uwumail-smtp/src/rules.rs:264` (`deliver`), `crates/uwumail-smtp/src/forward.rs:43`

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:L` (Medium, 5.4)
- **Impact:** a `redirect` without `:copy` counted as done the moment it was handed to forwarding,
  even when forwarding sent it nowhere (a `Delivered-To` loop, a local target that takes no mail, a
  missing SRS secret, a failed queue write). The implicit keep was cancelled and the message stored
  nowhere, though the sender heard 250 — silent mail loss, against the promise in docs/sieve.md.
- **Fix:** `forward::send` returns whether a target was reached; a redirect that reached nobody
  keeps the message in the inbox.
- **Regression test:** `crates/uwumail-smtp/tests/flow.rs` — a redirect of a looped message keeps
  the mail; without the loop it goes out.

### S-45 · Medium · The calendar work of one request was unbounded

`crates/uwumail-jmap/src/methods/calendar_event.rs`, `crates/uwumail-jmap/src/jscal.rs`

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:U/C:N/I:N/A:H` (Medium, 6.5)
- **Impact:** the query's five-second expansion limit was per method call and reset for each of the
  64 calls a request may carry; `CalendarEvent/get` with instance ids and `CalendarEvent/set` had
  no time limit at all. Checking an event with many `recurrenceOverrides` built each changed
  instance from a full clone of the series, overrides included — work growing with the square of
  their number — on the worker threads.
- **Fix:** all calendar-event calls of a request share fifteen seconds counted from `Ctx.started`;
  the query checks its deadline per instance and per override; a series may have at most 1000
  overrides; `instance()` no longer copies the overrides; and checking and converting an event runs
  on the blocking pool.
- **Regression test:** `crates/uwumail-jmap/tests/calendars.rs` — a series of 1001 overrides is
  refused, one of 1000 is stored and found.

### S-46 · Medium · A blob was read whole before its size was checked

`crates/uwumail-jmap/src/methods/sieve.rs:47` (`content`),
`crates/uwumail-store/src/blobs.rs` (`blob_size`)

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:U/C:N/I:N/A:L` (Medium, 4.3)
- **Impact:** `SieveScript/set` and `/validate` read the whole blob a client named — which may be
  an upload or a message of up to 50 MB — into memory before refusing it as too large, and one call
  may name the same blob for every create.
- **Fix:** the recorded size is checked first; a blob larger than the script limit is never read.
- **Regression test:** `crates/uwumail-jmap/tests/sieve.rs` — 50 creates and a validate naming an
  over-size blob whose bytes were removed are all `tooLarge`, answered from the size alone.

### S-47 · Medium · A ManageSieve connection could stay open without ever logging in

`crates/uwumail-imap/src/managesieve.rs:287` (`read_limit`), `:469` (`authenticate`)

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:L` (Medium, 5.3)
- **Impact:** before logging in only each single command had a one-minute timeout, so a `NOOP` now
  and then held a connection for ever, and the answer to `AUTHENTICATE`'s empty challenge was read
  with no timeout at all. Each of the 500 connection slots could be tied up by one client with no
  password.
- **Fix:** a connection has three minutes in all to log in (again after `UNAUTHENTICATE`), min'd
  into the per-command timeout, and the challenge's answer is read under the same clock.
- **Regression test:** `crates/uwumail-imap/tests/managesieve.rs` — a NOOP loop and an unanswered
  challenge are both closed within the shortened budget.

### S-48 · Medium · `fileinto :create` let one message make many folders

`crates/uwumail-smtp/src/rules.rs:176` (`folder_for`)

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:L` (Medium, 5.4)
- **Impact:** `fileinto :create` takes its folder name from the script, and with `variables` from
  the message itself (`${1}` of a header). With up to 64 actions and 64 levels each, a sender could
  have every message create a large number of folders in the recipient's account.
- **Fix:** one message may create at most ten folders; a filing beyond that goes to the inbox.
- **Regression test:** `crates/uwumail-smtp/tests/flow.rs` — a 20-level name makes at most ten
  folders and the message stays in the inbox; a short name still gets its folders.

### Correctness · Exact durations were added to the wall clock, not the instant (fixed)

`crates/uwumail-jmap/src/jscal.rs:326` (`span`)

Found by the webmail end-to-end run. `span` added a duration's hours, minutes and seconds to the
local start and converted afterwards, so an event over the night the clocks change ended an hour
off in `utcStart`/`utcEnd`, in query windows and for expanded instances. Days and weeks stay
nominal (they move the wall clock); the exact part is now added to the UTC instant, per RFC 8984
section 1.4.6 and RFC 5545 section 3.3.6. Test in `calendars.rs` covers `/get`, non-expanded and
expanded query windows around the changed end.

## Low / Informational (not fixed)

- **L-1 · Low · Control characters from JMAP reach the account's own CalDAV XML.**
  `jscal::validate` bounds a `title`'s length but not its control characters (a `uid` does reject
  them). A JMAP event title containing e.g. U+0001 is stored and the account's own
  CalDAV `calendar-query` REPORT emits it raw, which is not well-formed XML 1.0 and can break that
  one account's own CalDAV clients. No cross-account effect and no server harm; the writer only
  hurts their own feed. Fix would be to refuse control characters in event text as `uid` does.

## What was run

- **Dynamic, against the running branch build:** cross-account IDOR sweep over Calendar,
  CalendarEvent (stored ids, synthetic instance ids), ParticipantIdentity and SieveScript get/set/
  destroy and the activation/default hooks; foreign and message blob ids as script source and the
  script download URL; the ManageSieve pre-auth surface (STARTTLS pipelining, SASL authzid,
  UNAUTHENTICATE); and the S-42 memory-growth measurement. Repro notes in the gitignored
  `security-test-notes.local.md`.
- **Static:** full read of the new modules and the vendored `sieve-rs 1.0.2` and `calcard 0.3.14`
  matching, glob and recurrence-expansion code.
- **Build:** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings` and
  `cargo test --workspace -- --test-threads=1`, all green, with the eight new regression tests.

## What could not be tested

- **A real reverse-proxy / gateway deployment.** ManageSieve is not carried through the tunnel yet
  (documented), so only the direct listener was exercised.
- **The parsers were not fuzzed** in this pass; calcard's recurrence expansion and the iCalendar
  parser are only bounded here by the 10 000-occurrence and time limits, read from the code.
- **CalDAV clients (phones, Thunderbird) against JMAP-written events** were not run; the JMAP↔CalDAV
  bridge was checked by the shared `check_calendar` path and by reading the store, not with a real
  client, apart from the raw REPORT used for L-1.
