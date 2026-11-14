# Security audit — 0.35.0, the new update system

A pass over what 0.35.0 adds: the update run in the portal's API (`uwumail-web`: `update_run.rs`,
`routes/updates.rs`, `host.rs`), the words the machine's helper now takes along with an update
(`deploy/host/helper`, `uwumail-server/src/host.rs`), `update.sh`'s new exit code, and the update
page (`web/src/features/updates`). The gateway, its helper and the tunnel did not change and were
not looked at again.

Done with Claude, not an independent firm; an honest sweep, not a certificate. It was a reading of
the code, not a test against a running machine. Step-by-step exploits are left out.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 2 | 2 | 0 |
| Low | 4 | 3 | 1 |

## Threat model

Unchanged, with the one that matters most here: **a server container somebody broke into must not
become root on the machine or on the VPS**. Everything the helper reads from the directory it
shares with the container is the attacker's to write, and so is `machine.json`, which the portal
shows. Second: **a signed-in admin's browser** left open, which is why starting a run asks for the
password again.

## Findings

### U-1 — The command to copy trusted a path from `machine.json` (medium, fixed)

- Place: `web/src/features/admin/host.tsx`, `updateCommand`.
- Cause: where there is no helper that can update, the page shows `cd <dir> && sudo bash update.sh`
  to paste into a root shell. The directory came from `machine.json`, which lies in the shared
  directory and can be rewritten from inside the container. The code is older than this release,
  but the update page is now the one place that shows it.
- Fix: only a plain absolute path (letters, digits, `.`, `_`, `-`, `/`) is used; anything else
  falls back to `/opt/uwumail`. Covered by `host.test.ts`.

### U-2 — A step whose helper died stayed "running" for ever (medium, fixed)

- Place: `crates/uwumail-web/src/update_run.rs`, `walk` and `follow_step`.
- Cause: the 50-minute limit only counted while no job with the step's id was found. A helper that
  wrote `running` and then died held the run, and with it every later one, since only one run
  exists at a time and only a failed step takes an answer.
- Fix: the limit counts for every step that is still running; it then fails with `timeout` and can
  be retried, skipped or stopped.

### U-3 — Skipping went on without the password (low, fixed)

- Place: `crates/uwumail-web/src/routes/updates.rs`, `step`.
- Cause: skipping a failed step starts the next one, which can be a restart confirmed long before.
- Fix: every answer but *Stop* asks for the password the way starting a run does.

### U-4 — The log limit counted characters (low, fixed)

- Place: `update_run.rs`, `tail`.
- Cause: 16 384 characters per step can be four times as many bytes, kept in one setting that is
  rewritten while a log moves.
- Fix: the limit is 16 KiB in bytes, cut at a character boundary.

### U-5 — Looking for a new system release had no time limit (low, fixed)

- Place: `deploy/host/helper`, `cmd_machine`.
- Cause: `do-release-upgrade -c` asks the network and ran as root after every job without a limit.
- Fix: `timeout 30`.

### U-6 — A job can be asked twice if the server dies in between (low, accepted)

- Place: `update_run.rs`, `walk` → `save_run`.
- Cause: the job is handed to the helper first and the step is saved as running right after. A
  server killed between the two finds the step pending again and asks once more.
- Why accepted: the two happen in the same call without anything in between, and every job is one
  that is safe to run twice (installing updates, updating to the newest release). A restart step
  asked twice is, at worst, a second restart the admin had confirmed.

## What held up

- The helper checks id, verb, `channel` (`latest`, `beta`, `edge`) and `compose` (`keep`,
  `replace`) against its own lists and turns them into fixed flags; no word from the container
  reaches a shell, a path or an address. The server checks the same lists before writing the job.
- The name of a new system release is cut down to letters, digits, dots and spaces before it is
  written, and the portal renders it, like every log, as text.
- All routes are for admins only; starting a run, and so every restart, needs the password again.
  Restarts are never part of "everything".
- One run at a time: start, advance and answer take the same lock. A setting that cannot be read
  is no run, not a panic. Job ids are compared exactly, so another job is never taken for a step.
- The commands the page offers for copying are otherwise built from the server's own constants.
