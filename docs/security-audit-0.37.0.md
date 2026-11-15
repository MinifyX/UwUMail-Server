# Security audit — 0.37.0, switching shipments off and the host override

A pass over what 0.37.0 adds: the admin's switch `shipments.enabled` for the whole server and the
account's own `shipments` setting as a hard off (`uwumail-store/src/shipments.rs`, `lib.rs`;
`uwumail-jmap`: `session.rs`, `api.rs`, `methods/mod.rs`, `push.rs`; `uwumail-assist/src/shipments.rs`;
`uwumail-server`: `config.rs`, `serve.rs`, `settings.rs`; `uwumail-web`: `settings.rs`,
`routes/auth.rs`; the portal's `ShipmentsCard` and admin settings), the matching changes in
UwUMail-Webmail (`feat/shipments-off`: `ShipmentsSetting`, `shipmentsSupport`, gating of the view
and the chip) and UwUMail-Client (`feat/shipments-off`: `Session::shipments_by`,
`shipment_ops.rs`, `AccountShipments`), and `deploy/host/install.sh` writing the `/host` mount into
a `compose.override.yaml` the admin wrote themselves (`deploy/host/add-mount.awk`,
`deploy/tests/host-override.sh`).

Done with Claude, not an independent firm; an honest sweep, not a certificate. It was a reading of
the diffs and the code around them, not a test against a running machine. Step-by-step exploits
are left out.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 0 | 0 | 0 |
| Low | 5 | 5 | 0 |
| Info | 5 | 0 | 5 |

Nothing blocks the release on its own. S-1 to S-5 (install.sh, runs as root) are fixed before the
tag: the override is refused when it is a link, the copy goes to a fresh 0600 file in
/var/lib/uwumail-host and only that copy is ever restored, `--service` must look like a compose
service name and is compared as a string, nothing is changed without docker to check it, a direct
child of `services:` is the only match and an inline `volumes: [...]` gives up, `--remove` keeps a
`volumes:` key that has items of the admin's own under it (remove-mount.awk), and the "already
there" check only counts a list item. deploy/tests/host-override.sh covers these.

## Threat model

First: **someone with a share or an app password** on an account. With shipments off (by the
admin or for the account) they must get no shipment data from any method, push or session, and a
sharee must not be able to flip the owner's switch or trigger the deletion. Second: **an account
holder who is not an admin**; they must not reach the server-wide switch. Third: **whoever can
write the directory UwUMail's compose files are in**, against `install.sh`, which runs as root and
now edits a file there. A writer of that directory already controls what root's `docker compose up`
starts, so findings there are defense in depth and robustness, not a new way to root.

## Findings

### S-1 — install.sh writes through a symlinked override or backup (low, fixed)

- Place: `deploy/host/install.sh:273–291` (`cp -p "$override" "$backup"`, `cat "$merged" >"$override"`,
  the rollback `cat "$backup" >"$override"`); `:140–142` (`sed -i` in `--remove`).
- Cause: `[ -f ]` and every write follow symlinks. If `compose.override.yaml` is a symlink, root
  rewrites its target; if `compose.override.yaml.before-uwumail-host` already exists as a symlink,
  `cp -p` overwrites whatever it points to with the override's content (which may hold secrets in
  `environment:`). The backup also lands in a directory root does not own. `sed -i` in `--remove`
  replaces a symlink by a regular file.
- Fix: refuse (print the lines to add instead) when the override is a symlink or not a regular
  file; keep the backup under root's own `$state` (0700 directory, file 0600, made with
  `mktemp`), not beside the override; in `--remove` skip a symlinked file as well.

### S-2 — A stale backup can be restored over newer edits (low, fixed)

- Place: `deploy/host/install.sh:280–287`.
- Cause: the backup name is fixed and is never removed after a successful run (nor by `--remove`).
  When a later run gets past `awk` but `cp -p` fails (full disk, read-only file), the `else`
  branch finds the old backup and writes it over the current file, taking back every change the
  admin made since. The warning then says the file "was left as it is".
- Fix: remove a leftover backup before `cp -p`, or better use a fresh `mktemp` backup (S-1) and
  restore only from the one this run made; drop the backup after a successful check, or tell the
  admin where it is and remove it in `--remove`.

### S-3 — `--service` is used unchecked in a regex and in YAML (low, fixed)

- Place: `deploy/host/install.sh:44`; `deploy/host/add-mount.awk:19` (`"^ +" svc ":"`); the
  generated file in `install.sh:250–257`.
- Cause: the value goes into an awk regex (metacharacters such as `|` or `.` change what matches,
  `-v` also turns backslash escapes into characters) and, in the file install.sh writes itself,
  straight into YAML. A wrong match puts the line into the wrong block; `docker compose config`
  catches that only when docker is on `PATH`. It is the root operator's own argument, so no
  boundary is crossed, but a typo can leave a broken compose file on a host without the docker CLI.
- Fix: accept only `^[A-Za-z0-9][A-Za-z0-9_.-]*$` and stop otherwise; in awk compare the key as a
  string (`substr`/`index` after stripping the indent) instead of building a regex.

### S-4 — The success path is not checked without docker, and the match is first-found (low, fixed)

- Place: `deploy/host/install.sh:282` (`! command -v docker ||`); `deploy/host/add-mount.awk:19`.
- Cause: without the docker CLI the merged file replaces the admin's file unchecked. The awk
  takes the first `<svc>:` line at any depth under `services:`, so a nested empty key of that
  name in an earlier service wins; an inline `volumes: [...]` is not seen, and a second `volumes:`
  key is added (a duplicate key).
- Fix: without docker, do not edit: print the lines as before. In awk, accept the service key only
  at the indentation of the first key under `services:`, and give up (exit 1) on an inline
  `volumes:` value.

### S-5 — `--remove` can leave a broken file, the "already there" check matches comments (low, fixed)

- Place: `deploy/host/install.sh:140–142` and `:273`.
- Cause: when install.sh added the `volumes:` key itself (marked), and the admin later put their
  own items under it, `--remove` deletes the key and leaves the items orphaned. `grep -qF
  "$bridge:/host"` also counts a commented-out line, so the mount is reported as present while
  docker does not see it.
- Fix: in `--remove`, delete the marked `volumes:` line only if no unmarked item follows it at its
  item indent (otherwise only the marked item, and say so); for the presence check, ask `docker
  compose config` (the same jq filter) instead of grepping.

### S-6 — Every JMAP request reads the switch of each shared account (info, open)

- Place: `crates/uwumail-jmap/src/api.rs:166–173`; `crates/uwumail-jmap/src/session.rs:436`
  (`shipment_switches`).
- Cause: for `sessionState`, each API request does one settings read for the account and one per
  account shared with it. Correct, but on top of `shared_accounts` it grows with the number of
  shares for every request.
- Fix: none needed for 0.37.0; if it shows up in profiles, read all switches in one query
  (`WHERE account_id IN (...)`).

### S-7 — The server switch is checked before the write, not inside it (info, accepted)

- Place: `crates/uwumail-store/src/shipments.rs:651` (`record_shipment_mail`).
- Cause: the atomic flag is read before the write transaction; a delivery that passes it just as
  the admin switches off still records its shipment.
- Why accepted: off keeps stored data anyway, so the row is hidden like every other one and the
  contract holds. The account switch, which deletes, is checked inside the transaction
  (`record_mail` → `enabled`) and so cannot race with the deletion in `follow_setting`.

### S-8 — Clients learn of the admin's switch only on their next request (info, open)

- Place: `crates/uwumail-server/src/settings.rs:88`; `crates/uwumail-jmap/src/push.rs:342`.
- Cause: flipping `shipments.enabled` sends no `StateChange`; an idle client keeps showing the
  parcels it already loaded until its next API call carries the new `sessionState`. No new data
  can be fetched meanwhile (`Shipment/*` is refused at once).
- Fix: optional: push a `StateChange` with `Shipment` (or the session) to connected listeners when
  the flag changes.

### S-9 — Parcels kept on the device stay after the server switched them off (info, open)

- Place: UwUMail-Client `crates/uwumail-core/src/engine/shipment_ops.rs` (`shipments_source`,
  `Source::Off`, `Source::Local` on timeout).
- Cause: `Source::Off` hides local parcels but does not delete those found by the app earlier;
  when the server cannot be reached within 8 s the account falls back to `Local`, so a parcel can
  be added by hand on the device for an account whose server has them off (hidden again once the
  server answers).
- Fix: on the switch to off (`shipments_switched`), also clear the account's local shipments;
  treat an unreachable account whose last session said `Off` as `Off`.

### S-10 — Test gaps on the new paths (info, open)

- Place: `crates/uwumail-jmap/tests/integration/shipments.rs`; `deploy/tests/host-override.sh`.
- Cause: no test that a shared account loses `urn:uwumail:jmap:shipments` and gets
  `accountNotSupportedByMethod` when its owner switches off, that push leaves `Shipment` out while
  off, or that a sharee cannot write the owner's `UserSettings`. The host test does not cover a
  symlinked override or a failing write.
- Fix: one integration test for the share (owner off → sharee session and `Shipment/get`), one
  push assertion, and the S-1/S-2 cases in `host-override.sh`.

## What held up

- Authorization: `shipments.enabled` is a setting of `/api/admin/settings` and is applied only
  through `SettingsBackend::apply`; the person's switch is written through `UserSettings/set` and
  `/api/account/preferences`, both for the own account only. `UserSettings` and `Shipment/rescan`
  are not among the shared methods (`sharing.rs`), so a sharee can neither flip the owner's switch
  nor start the deletion or a rescan.
- `dispatch` checks `shipments_enabled` after `sharing::enter`, so a call into a shared account is
  judged by the owner's switch and the server's; `shipments_enabled` folds in the server flag, so
  every caller of it (delivery in `uwumail-smtp/src/labels.rs`, the AI fallback in
  `uwumail-assist`, push) is off with it. The model is not asked when off; the rescan loop waits.
- Session: server off removes the capability from `capabilities`, every account and
  `primaryAccounts`; account off only from that account (and `primaryAccounts` for the own one);
  shared accounts copy the own capability before `keep_shipments` runs and are then cut by their
  owner's switch. Masked-only sessions never had it. `sessionState` (session and API) changes with
  either switch.
- Push: `type_states` drops `Shipment` while off; shared accounts never carried it.
- No new `unwrap` on request paths: the switch reads fall back to off with a warning, the JSON
  edits use `as_object_mut` and index only accounts that are in the document.
- Turning the account off still deletes its shipments and rescan in the settings transaction;
  with the server off the setting can still be written and the deletion still happens; switching
  back on queues a rescan that waits for the server switch.
- UI: portal, webmail and desktop ask before switching off (the deletion is named, Cancel has the
  focus); the webmail and desktop call no `Shipment/*` method when the session lacks the capability
  for the own account, leave the view, hide the chip, the command and the section, and drop the
  cached list. The portal card disappears when the admin switched parcels off.
- install.sh: the merged file is written over the existing inode (`cat >`), so owner and mode of
  the admin's file stay; `cp -p` keeps the backup's mode no wider than the original; nothing from
  `docker compose config` is printed; awk gives up on flow style and a missing service, and a
  failed check puts the original content back.
