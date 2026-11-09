# Security audit — release 0.30.0

Scope: everything new between `main` and `feat/codes-0.30.0` (`git diff origin/main...HEAD`,
lockfiles left out, commit c4dbd2c): one-time code and sign-in link detection
(`crates/uwumail-store/src/verification.rs`, `verification_codes.rs`), detection for locally
submitted, forwarded and redirected mail (`crates/uwumail-smtp/src/submission.rs`, `forward.rs`,
`rules.rs`, `inbound.rs`), the new trust levels `dkim_aligned` / `spf_aligned` (`checks.rs`,
`spam.rs`), alphanumeric and grouped codes, more languages, re-assembly of per-digit cells
(`lines_of`), and the one-time re-scan of the last 24 hours (`crates/uwumail-assist/src/rescan.rs`,
`crates/uwumail-server/src/serve.rs`, `verification_rescan_candidates`). Read only, nothing built
or run.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 0 |
| Medium | 1 |
| Low | 3 |
| Info | 4 |

The re-scan's trust boundary holds. It reads only mail with `smtp_delivered = 1`. It takes the
`Authentication-Results` only from the block above the second `Received`, and only when the topmost
`Received` is this server's (`trusted_headers`). In that block it uses the first header whose
authserv-id is this host. A sender's copy of that header is stripped on inbound and on submission
(`strip_forged_auth_results`). IMAP APPEND and JMAP import are never marked as delivered. A
malformed value is discarded (`well_formed`). Values that mail-auth writes are quoted, so a MAIL
FROM can't smuggle in a `dkim=pass`. If the hostname changed, the re-scan finds nothing, so it
fails closed. Submission strips `Verification-Info` and never reads it, so VC-01 holds. On
submission, every From and Sender address must belong to the account.

The new regexes run on the Rust `regex` crate (linear time) over at most `MAX_TEXT` (16 KiB).
`lines_of`, `code_of` and `plausible` are linear, slice only within bounds and don't unwrap on
input. Re-assembly copies the cut text at most about twice. `MAX_MESSAGE` applies on inbound, on
submission and in the re-scan (`record.size`, then `detect`).

The one Medium finding (R30-01) was the new submission path. Every local account that may send as an
address under a domain it shares with other accounts got codes and sign-in links filled in for that
registrable domain. This covered mail domains with several people, masked-only domains and
subdomains of the server's own domain. The Low findings covered the new DKIM/SPF levels and the side
effects of the re-scan. All findings were fixed before the release.

Earlier properties:
- VC-01, VC-02, VC-04 and VC-06 to VC-09 still hold.
- VC-02: the fast lane is still for signed headers only.
- VC-08: the re-scan judges a code against `received_at`.
- VC-04: the re-scan pushes only codes that are still valid, within `CodeBudget`.
- VC-03 was changed on purpose. A guess is now also made on an aligned DKIM pass, or on SPF alone
  with `autofill` off. The store no longer refuses a guess that isn't for autofill, so the caller
  alone decides (R30-02, R30-03).
- VC-05 still holds by its rule (only header codes or `autofill = 1`), but more mail now has
  `autofill` set (R30-01, R30-04).

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| R30-01 | Medium | `crates/uwumail-smtp/src/submission.rs` `submit_checked` (verification block), `deliver_locally` | Submitted mail counts as `dmarc_passed` and `dkim_aligned`, and the origin of a guess is the registrable From domain. Any account that may send as an address under a domain it shares with other accounts can get a code or `https` sign-in link recorded with `autofill: true` for that registrable domain. That domain may host the server's own login (webmail, UwUAuth, apps on the same domain). Such domains include a mail domain with several people, a masked-only domain every user has addresses on, or a subdomain of the server's own domain. Local delivery skips the recipient's spam filter, so the code is always pushed. The clean-up also moves such mail to Trash later. | A submitted code gets `autofill` only when the sending account is the only one with addresses on the registrable From domain (`Store::sole_sender_on`: addresses and aliases on the domain and its subdomains, masked addresses, send-as grants, shared-mailbox members and groups that may send as them; a masked-only domain or one handed out for masked addresses never qualifies). Otherwise it counts like SPF alone: shown, not filled in, not pushed, not cleaned up. `shortcut:` comment names a service-account flag as the upgrade path. Tests `a_code_from_an_app_on_this_server_is_found` (an app alone on its domain vs. two people on one) and `only_the_sole_sender_on_a_domain_is_its_own`. | fixed |
| R30-02 | Low | `crates/uwumail-smtp/src/checks.rs` `verify` (`dkim_aligned`, `spf_aligned`); `crates/uwumail-assist/src/rescan.rs` `authenticity` | `dkim_aligned` and `spf_aligned` use relaxed alignment by registrable domain even when the domain publishes DMARC and DMARC failed. With `p=none` and `adkim=s`/`aspf=s`, the mail is delivered and not marked as junk. A signer on a delegated subdomain (for example a third-party sender) then gets codes filled in for the parent's origin, against the domain owner's strict policy. The re-scan doesn't check `dmarc=fail` either. | `dkim_aligned` and `spf_aligned` count only when the From domain has no DMARC record (`dmarc_record().is_none()`; in the re-scan `dmarc=none` or none at all). With a record the DMARC result decides. Test `alignment_without_dmarc_only_counts_when_there_is_no_record` (`aspf=s` refuses, no record shows); the re-scan test covers `dmarc=fail`. | fixed |
| R30-03 | Low | `crates/uwumail-store/src/verification.rs` `find`; `verification_codes.rs` `record_verification`, `within_limit` | A guess on SPF alone is now kept, pushed and labelled with the From domain as origin. On a shared sending service with a loose SPF record (or a HELO identity for a null sender), another customer can get such a code shown under a foreign origin. This applies to domains without DMARC. These rows also count toward the 10-per-hour share per sender domain. 10 SPF-only mails can therefore use up the hour, and the real, DKIM-signed codes of that domain are then dropped. | Guesses without `autofill` are shown but never broadcast, so no Web Push. `within_limit` counts per bucket (`autofill`), so they have their own share per sender and cannot use up the real codes'. Test `shown_guesses_and_rescanned_codes_keep_out_of_the_share_of_real_codes`. | fixed |
| R30-04 | Low | `crates/uwumail-assist/src/rescan.rs` `rescan_one`; `verification_codes.rs` `record_verification`, `clean_up_account` | The re-scan records codes from the last 24 hours with `created = now()`. A sender with many codes that day uses up the hour's share, and its new live codes after the update aren't recorded. Codes that have expired since get `autofill = 1` rows. The clean-up then moves mail the person has already seen and kept to Trash. This includes false positives of the new heuristics (grouped codes, re-assembled cells, new languages). With the clean-up set to `immediately`, it moves a whole day's mail in bulk at the next run. | `record_rescanned_verification` keeps only codes still valid at re-scan time and marks them `rescanned` (migration 0088); `within_limit` leaves them out. Expired codes are no longer stored, so the clean-up does not move old mail. Same test: a backlog of 10 re-scanned codes, then a live code that is kept and pushed. | fixed |
| R30-05 | Info | `crates/uwumail-assist/src/rescan.rs` `run_verification_rescan`, `crates/uwumail-server/src/serve.rs` | The marker is `verification-rescan:<CARGO_PKG_VERSION>`, so every patch release runs the re-scan again, whether or not detection changed. This repeats the effects of R30-04, and one settings row is added per version. | The marker is `verification-rescan:<DETECTOR_VERSION>` (`verification::DETECTOR_VERSION = 2`), raised only when the detector changes. | fixed |
| R30-06 | Info | `crates/uwumail-store/src/verification_codes.rs` `verification_rescan_candidates` | `emails` has no index that starts with `received_at`. The first batch (`id > 0`) walks the table by rowid until it reaches the first mail of the last day, which is a full scan on a large store. Later batches are cheap. | The candidate query starts at the first id received since `since`, found per account through `emails_account_received` (one scalar subquery, rowid range from there). No new index. | fixed |
| R30-07 | Info | `crates/uwumail-store/src/verification.rs` `lines_of` | Any run of 4 to 8 neighbouring single digits or capitals is joined, wherever it comes from. An adjacent one-character cell next to the code cells, or a numbered list right below a code keyword, becomes part of the code or a code of its own. That gives a wrong code with `autofill`. On the path with one piece per line, the piece is kept with its trailing punctuation (`run.push(trimmed)`), so `4.` `8.` … finds nothing. | Only single digits join (letters never), trimmed of `.,;!:)` on both paths; a run joins only on lines of its own or as the trailing run of a line, and not when it counts up (`1 2 3 4`). Negative cases (numbered list, count, run in a sentence) in `a_corpus_of_other_numbers`; a neighbouring letter cell and `3.` on the last line in `a_corpus_of_codes`. | fixed |
| R30-08 | Info | `crates/uwumail-smtp/src/submission.rs` `submit_checked` | Detection (a copy of up to 1 MiB plus a parse on the blocking pool) runs for every submitted mail, also when no recipient is local and the result is never used. | Detection runs lazily (`OnceCell`) at the first local delivery or forwarding address; mail that only leaves is not parsed. | fixed |
