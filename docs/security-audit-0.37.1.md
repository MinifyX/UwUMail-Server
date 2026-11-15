# Security audit — 0.37.1, reading shop mail better

A pass over what 0.37.1 changes (`git diff main...release/0.37.1`): the shipment rules
(`uwumail-labels/src/shipment.rs`: plural shipping words, `is_ticket`, Amazon's own tracker as
`tracking_url`, „ankunft“ as an expected-day word), `Mail::parse` reading the HTML part when the
text part is a placeholder (`uwumail-labels/src/mail.rs`), matching a shop by a name that starts
the other (`uwumail-store/src/shipments.rs`, `record_mail`) and migration 0094, which removes
orders only mail made and queues a look back.

Done with Claude, not an independent firm; a reading of the diff and the code around it, not a
test against a running machine.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 0 | 0 | 0 |
| Low | 0 | 0 | 0 |
| Info | 3 | 0 | 3 |

Nothing blocks the release.

## Findings

- **I-1 HTML part read for a short text part** (`mail.rs`). Mail is attacker input. The HTML is
  converted with mail_parser's `html_to_text`, linear in its size, and only for a text part under
  64 characters; the raw mail is already cut to `MAX_PARSE_BYTES` before parsing and parsed off
  the async runtime. The longer text feeds every rule, not only parcels; the rules already handle
  HTML-only mail the same way. Accepted.
- **I-2 Shop names by prefix** (`record_mail`). A sender can choose its display name, so „Apple“
  joining „Apple Store“ lets a mail join another shop's order. It still needs the exact order
  number of an existing order in the same account, as before with a missing shop name, and the
  rules for unauthenticated mail (no ending a known parcel) are unchanged. Accepted.
- **I-3 Amazon tracker link** (`shipment.rs`). Taken only from an https link whose host is one of
  Amazon's carrier domains and whose path is the progress tracker, the same check carrier links
  without a number already had; redirects are followed only through the existing bounded loop.
  Migration 0094 deletes only rows without a number, name, hiding or app event, records each as
  `destroyed` with its own modseq, and the look back reads only the account's own mail. Accepted.
