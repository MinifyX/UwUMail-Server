# Security audit — 0.38.1, stricter parcel rules, `SUBJECT_HAS_RECIPIENT`, migration 0099

Scope: the change since 0.38.0 on `fix/shipments-reread-0381`. That is `tracking` and `dhl_ok` in `crates/uwumail-labels/src/detect.rs`, `shipment_of` in `crates/uwumail-labels/src/shipment.rs`, the model skip in `crates/uwumail-assist/src/shipments.rs`, the new rule in `crates/uwumail-smtp/src/spam/content.rs` and migration `0099_shipments_reread_again.sql`. The ignored corpus test and the docs were not reviewed.

Done with Claude, not an independent firm; it reads the code and does not replace a penetration test. Nothing was run for the review; the fixes were tested afterwards.

## Summary

| Severity | New | Fixed | Accepted / open |
|---|---|---|---|
| High | 1 | 1 | 0 |
| Medium | 2 | 2 | 0 |
| Low | 1 | 1 | 0 |

The threat model is the one in `docs/security-audit-0.38.0.md`: a stranger controls everything in an unauthenticated mail.

## Findings

### S-1 — `SUBJECT_HAS_RECIPIENT` did unbounded work (high, fixed)

- **Where:** `headers` in `spam/content.rs`.
- **Cause:** every To address was searched for in the whole subject. Header fields have no length limit, so a long subject and a long To list multiplied.
- **Fix:** the subject is clipped to `MAX_SUBJECT_CHARS` as in `phishing.rs`, at most 50 addresses are looked at, and an address longer than the subject is skipped.

### S-2 — Ruled-out parcel mail skipped the carrier cross-check (medium, fixed)

- **Where:** `shipment_of`.
- **Cause:** the new exclusions returned nothing, so delivery never asked the carrier about the mail's numbers and never junked a phishing mail about a parcel the carrier does not know. That covered a carrier's name on another domain, fees, "verify your …", the recipient's address in the subject, and a number in mail about something else. The model was then also asked about these mails and could still make a card.
- **Fix:** these mails return their numbers with confidence 0. That means no card, and the model is skipped (`uwumail-assist`). An unauthenticated mail's labelled number is read even without a second sign, so the carrier is still asked about it.

### S-3 — An unauthenticated From on a carrier's domain counted as the carrier (low, fixed)

- **Where:** `carrier_sends` and `carrier_writes` in `shipment_of`, and `from_carrier` in `tracking`.
- **Cause:** all three checked only the From domain.
- **Fix:** all three now need an authenticated From.

### S-4 — Migration 0099 could delete parcels for good (medium, fixed)

- **Where:** `0099_shipments_reread_again.sql`.
- **Cause:** the rescan rebuilds a card only from mail that still exists outside Junk. A card whose mail was deleted would have gone for good, along with its carrier history.
- **Fix:** cards with a mail event whose mail is gone or in Junk stay. A test covers this.

## Checked without findings

- **Byte slicing in `tracking`:** the look-back stops on a char boundary.
- **`dhl_ok`:** checks the length before it indexes.
- **`order_number`:** its bounds are safe.
- **Recipient check in `shipment_of`:** it is bounded (50 recipients, a clipped subject).
- **Migration 0099:** it keeps named, hidden and app-added cards, its foreign keys cascade, and its modseq handling matches 0094.
