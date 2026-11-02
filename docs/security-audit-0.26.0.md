# Security audit — verification codes (0.26.0)

Scope: `git diff main..feat/verification-codes` (6 commits): store
(`verification.rs`, `verification_codes.rs`, migration 0081, user settings),
SMTP (`checks.rs`, `dkim.rs`, `inbound.rs`), JMAP (`Email/get` `verification`,
`VerificationCode/query`, Web Push), the cleanup task in `serve.rs` and the
portal preferences.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 1 |
| Medium | 1 |
| Low | 5 |
| Info | 4 |

The parser has no panic paths. Strings are cut only at character boundaries,
and indexing is checked. Header length and field count are limited. The
heuristic reads at most 16 KiB, and the regex engine is linear. Expiry is
clamped to 24 h, and the push TTL is clamped.

There is no cross-account access: all writes and queries are scoped by
`account_id`, and `VerificationCode/query` is not in `SHARED_METHODS`. A code
is pushed only to the owner's own subscriptions, and only to those that are
encrypted and explicitly ask for the `VerificationCode` type.

Status of each finding: see the column "Status".

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| VC-01 | High | `uwumail-smtp/src/dkim.rs` `SIGNED_HEADERS`, `submission.rs` | Submission does not strip `Verification-Info`, but the DKIM signer covers it. Any user allowed to send from a hosted domain can therefore send a header that the domain key vouches for, so receivers set `autofill: true` for any origin under that domain. | Strip the header on submission, the same way `Face` is stripped. Only senders that an admin explicitly allows may keep it and have it signed. | fixed: `strip_verification_info` on submission next to `strip_faces`; no per-account admin settings exist, so it is stripped for everyone (documented). Test `a_local_sender_cannot_send_a_verification_info_header`. |
| VC-02 | Medium | `uwumail-smtp/src/inbound.rs` (greylist fast lane), `uwumail-store/src/verification_codes.rs` | The rate limit counts only codes that were recorded. Mail sent to Junk, discarded or forwarded is never counted, and parallel sessions race past the check. One eligible recipient lifts greylisting for all recipients of the mail. The limit is per author domain only, so many domains multiply it. | Count every fast-lane pass at decision time. Lift greylisting only for the eligible recipient. Add a global per-account cap on fast-lane passes per hour. | fixed: passes are taken in one write transaction at decision time in `verification_fast_lane` (10/sender, 30/account per hour); greylisting is lifted only when every recipient got a pass. Tests in store and SMTP. |
| VC-03 | Low | `inbound.rs` verification block, `verification.rs` | Heuristic codes are recorded and pushed even without DMARC pass, labelled with the unverified From domain. Forged mail also uses up the real sender's quota. | Record or push heuristic codes only after DMARC pass, or keep them in a separate bucket with their own marking. | fixed: `detect` makes no guess without DMARC pass, the store keeps none either. |
| VC-04 | Low | `uwumail-jmap/src/webpush/mod.rs` `WebPush::code` | Code pushes bypass coalescing and spawn one task per code and subscription with high urgency. Combined with VC-03, anyone can flood a user's devices. | Cap code pushes per account per minute and per hour, and bound concurrency. | fixed: `CodeBudget` (20/min, 60/h per account) and a semaphore of 32 code pushes in flight. |
| VC-05 | Low | `verification_codes.rs` cleanup | The default 24 h cleanup also trashes heuristic codes from unauthenticated senders. False positives disappear, and phishing mail can clean itself away. | Clean up only header codes and DMARC-passed heuristic codes. | fixed: the cleanup moves only header codes and guesses with `autofill = 1` (DMARC passed). |
| VC-06 | Low | `verification_codes.rs`, migration 0081 | Rows are never deleted. With cleanup `off`, `cleaned` stays 0, so the job scans the whole table every 5 minutes and holds the writer. Switching from `off` to `immediately` moves an unbounded number of mails in one transaction. | Mark rows as done for `off` too, add a partial index `WHERE cleaned = 0`, work in batches, and delete rows after a retention period. | fixed: `off` (and accounts without Inbox/Trash) mark rows done; partial index `WHERE cleaned = 0` in 0081; batches of 500; rows deleted after 30 days. |
| VC-07 | Low | `verification_codes.rs` `query_verifications` | `expired: true/null` loads every id into memory before paging. | Page in SQL and bound the result by retention. | fixed: `LIMIT/OFFSET` in SQL with `COUNT(*)`, anchors resolved by `verification_position`. |
| VC-08 | Info | `verification.rs` | Codes that have already expired are still recorded and pushed. | Discard codes where `expires_at <= received_at`. | fixed: `detect` drops codes with `expires_at <= received_at`. |
| VC-09 | Info | `inbound.rs`, `verification::detect` | Every message is cloned and parsed again in full, and the first body part is converted completely before it is cut to 16 KiB. | Skip detection for large messages, or reuse the existing parse. | fixed: no detection for messages over 1 MiB (`verification::MAX_MESSAGE`). |
| VC-10 | Info | header path | A signed code or link mail can be forwarded to someone else and stays DKIM-valid (inherent to DKIM). | Document it. Optionally compare To/Delivered-To with the recipient. | documented in `docs/verification-codes.md`. |
| VC-11 | Info | `verification.rs` `is_code` | A code consisting only of hyphens (`----`) is accepted. | Require at least one alphanumeric character. | fixed: a code needs a character other than `-`. |

## Checked without findings

- The trust check requires: exactly one From, a DKIM pass aligned with From, `h=` including the header, DMARC pass, and exactly one `Verification-Info` header.
- The origin is bound to the From domain through the PSL. Uppercase, trailing dots and IDN are handled.
- Link URLs must be https only, with no userinfo, no backslash and no control or non-ASCII characters. The port is checked, and the host must lie under the origin.
- Shared-account path of `Email/get`.
- `wants_codes` requires keys plus an explicit type, never the "all types" setting.
- Codes never appear in logs.
- The portal preference whitelist and `validate_setting`.

Not checked: the mail-auth and mail-parser libraries, client rendering, i18n.
