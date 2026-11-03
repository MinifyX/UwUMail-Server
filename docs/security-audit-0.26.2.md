# Security audit — verification guess (0.26.2)

Scope: `git diff v0.26.1..fix/verification-guess`: `uwumail-store/src/verification.rs`
(HTML reading, written-out validity, sign-in link guess) and its docs.

## Summary

| Severity | Count |
| --- | --- |
| Critical | 0 |
| High | 0 |
| Medium | 1 |
| Low | 2 |
| Info | 4 |

The new HTML reader has no panic paths: offsets come from an ASCII-lowercased
copy of the same length, so every slice stays on a character boundary, and
unterminated comments, tags and elements end the walk. The input is cut to
1 MiB, and the walk stops past 16 KiB of text. All patterns run on the linear
regex engine. Guesses keep needing DMARC pass (VC-03), and a guessed link has
to be `https` below the From domain like a signed one.

Status of each finding: see the column "Status".

## Findings

| ID | Severity | Location | Cause | Fix | Status |
| --- | --- | --- | --- | --- | --- |
| VG-01 | Medium | `verification.rs` `guess_link` | Platform notifications (issue trackers, forums) pass DMARC for the platform, but their body is written by other users. A comment with a "Sign in" button and "didn't request" wording would become an autofill link of the platform. | Skip the link guess for list, bulk and reply mail. | fixed: `is_list_mail` (`List-Id`, `Precedence: list/bulk/junk`, `In-Reply-To`, `References`). Test `a_sign_in_link_is_only_believed_in_the_html_itself`. |
| VG-02 | Low | `verification.rs` `guess_link` | An open redirect on the sender's domain turns the guessed link into one to any site. | Refuse links whose query carries another absolute address. | fixed: `redirects` (`=http`, `=//`, encoded `://`). Same test. |
| VG-03 | Low | `verification.rs` `LINK_SIGNS` | "link ist" and "link is valid" matched ordinary newsletter wording. | Use narrower phrases. | fixed: "link ist nur", "link ist gültig", "link is valid for". |
| VG-04 | Info | `verification.rs` anchors | The anchor pattern ran over the raw HTML: `data-href` counted as `href`, and links in comments, `style`, `script` or `template` counted as visible. | Collect links in the same walk that reads the visible text, with `\shref`. | fixed: `read_html` returns text and links together. Same test. |
| VG-05 | Info | `verification.rs` anchors | Only `&amp;` was decoded in `href`, so `&#38;` left a different URL than a browser opens. | Decode entities in full. | fixed: mail-parser's entity decoder on the `href`. |
| VG-06 | Info | `verification.rs` HTML reader | The walk ran over the whole 1 MiB even though only 16 KiB of text is read. | Stop past `MAX_TEXT`. | fixed. |
| VG-07 | Info | `docs/verification-codes.md` | Forwarding (as for VC-10) also applies to guessed links. | Document it. | fixed. |

Accepted: guessed links take part in the expiry cleanup like guessed codes
(`autofill = 1` after DMARC pass, VC-05).
