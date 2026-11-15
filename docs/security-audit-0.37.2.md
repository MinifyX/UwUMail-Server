# Security audit — 0.37.2, parcels marked by hand and binned mail

A pass over what 0.37.2 changes (`git diff main...release/0.37.2`): `removeEvents` in
`Shipment/set` (`uwumail-jmap/src/methods/shipment.rs`, `uwumail-store/src/shipments.rs`:
`removals`, the delete in `update_shipment`, `replay`), the look back reading Trash
(`shipment_rescan_candidates`) with migration 0095, and `moves_to` letting a failed delivery end
delivered (`uwumail-labels/src/shipment.rs`).

Done with Claude, not an independent firm; a reading of the diff and the code around it, not a
test against a running machine.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 0 | 0 | 0 |
| Low | 0 | 0 | 0 |
| Info | 2 | 0 | 2 |

Nothing blocks the release.

## Findings

- **I-1 `removeEvents` input** (`removals`). Client input over JMAP. A list of at most
  `SHIPMENT_MAX_EVENTS`, each an object with only `at` and `status`, both parsed strictly; anything
  else is `invalidProperties`. The delete runs inside the account's update transaction on a
  shipment already looked up in that account, touches only `source = 'client'` rows, and the
  replay reads the shipment's own events once (bounded by the same event cap). Mail-made events
  cannot be removed. Any app of the account can take back another app's event; that is the
  account's own data. Accepted.
- **I-2 Trash in the look back** (`shipment_rescan_candidates`, migration 0095). Binned mail is
  the account's own mail; Junk stays excluded, so unwanted senders still can't make parcels through
  the look back. The migration only queues one 14-day look back per live account, merged with a
  running one; mails already read change nothing. Accepted.
