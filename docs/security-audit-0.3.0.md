# Security review — what 0.3.0 adds, 18 September 2026

A smaller pass than the three before it ([security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md), which went through the whole stack
earlier the same day). This one only looks at what came after that sweep:

- the **virus scanner**: `crates/uwumail-smtp/src/clamav.rs`, the two places that call it
  (`inbound.rs`, `submission.rs`), the header handling in `headers.rs`, the new spam-history
  entry, the portal page with its two API routes, the health check and the compose service;
- the **DNS records at Cloudflare**: quoting TXT values, reading back what is published there, and
  the new "bring the spelling in line" path that rewrites records that already work.

Done with Claude, not an independent firm. An honest sweep, not a certificate. Nothing was run
against the production server.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 0 | 0 | 0 |
| High | 0 | 0 | 0 |
| Medium | 0 | 0 | 0 |
| Low | 2 | 2 | 0 |

Two small ones, both fixed in the same commits. Three more things are deliberate and written down
under [Accepted](#accepted) so they are not rediscovered as findings later.

## Findings

### F-1 · Low · A scanner that hangs held up the portal's overview

`crates/uwumail-smtp/src/clamav.rs` (`Clamav::status`)

The health overview asks the scanner what it is on every request. That call used the same patience
as a scan — 30 seconds by default, and settable up to 300. A clamd that accepts the connection and
then says nothing (an overloaded container, a firewall that swallows packets, a wrong port that
happens to be open) would have held the admin's page for that long, on every load, for every admin.
Nobody outside can set this off: it takes the configured scanner misbehaving. But it turns one slow
service into a slow portal.

**Fixed.** The status probe now takes the shorter of the configured patience and five seconds
(`STATUS_TIMEOUT`). Scanning a message keeps the long one, where waiting is the right thing to do.

### F-2 · Low · A made-up date from the scanner could overflow the date arithmetic

`crates/uwumail-smtp/src/clamav.rs` (`built_at`)

clamd's `VERSION` line ends in the date its signature database was built, which the portal shows as
an age. The parser took the year, the day and the clock straight from that line and multiplied them
out. A line claiming the year 9223372036854775807 would overflow `i64`: in a debug build that is a
panic, and with `panic = "abort"` in release builds a panic is the whole server. Release builds wrap
instead of panicking, so the practical effect there was a nonsense date, not a crash — and it takes
the configured scanner to send it, which is why this is Low and not higher.

**Fixed.** The year, day and clock are bounded before anything is multiplied; a date outside them is
simply not a date, and the portal shows the version without an age.

## Checked and found in order

- **The virus name never escapes into somewhere it can break something.** clamd's own words end up
  in an SMTP reply (`554 5.7.0 This message contains …`), in the JMAP error and in the spam history.
  `plain()` keeps only printable ASCII and spaces and cuts at 80 characters, so a signature name
  carrying `\r\n550 …` cannot forge a second SMTP line, and the portal renders it through React,
  which escapes. There is a test for the CRLF case.
- **A sender cannot vouch for itself.** `X-Virus-Scanned` that comes with a message is stripped
  whenever our own scanner is switched on, the same way spam verdicts already were, so the only
  such header left is the one this server wrote. There is a test for it in `flow.rs`.
- **An error is never a clean verdict.** Every path that is not a clear "OK" from clamd ends in
  `Checked::Unchecked`, which delivers the message with `X-Virus-Scanned: no (…)`. The reasons in
  that header are fixed strings, never anything the scanner or the sender said.
- **Nothing new is reachable without logging in.** Both new routes take the `Admin` extractor, and
  non-GET requests go through the CSRF check in the session extractor. The self-test is an admin
  action and is written into the change log.
- **The scanner cannot be used to read the message back out.** The message is streamed to clamd and
  nothing of it is kept on our side; the reply is bounded to 4 KiB and the whole exchange to the
  configured timeout, so a hostile scanner cannot make the server read forever.
- **The EICAR string is assembled at runtime**, so the binary and this repository do not carry a
  signature that other people's scanners would flag.
- **The spam history gained one field of content**: the name of what was found. No message body,
  no attachment, nothing else is stored, and virus entries follow the same retention as the rest.
- **Cloudflare quoting.** `quote_txt` escapes `"` and `\` and splits at 255 bytes, so a value cannot
  break out of the string it is written into; `unquote_txt` is a small state machine with no
  indexing and no panics, over data that comes from Cloudflare's own API answer.
- **The count query for the virus page is parameterised** like every other query in the store.

## Accepted

These are deliberate, and the reasons are written down so they stay deliberate.

- **The scanner fails open.** If clamd is away, too slow, or the message is bigger than it takes,
  the message is delivered with a header saying nobody looked. The alternative — a temporary
  refusal — would mean a scanner outage quietly stops the post, which for a server a family or a
  club runs is worse than the risk it avoids. The health overview turns red while the scanner is
  unreachable, and the server log says why for each message.
- **An admin can point the scanner anywhere.** `spam.antivirus.address` is a host and port an admin
  types in, and every message is sent there. That is the same trust an admin already has over the
  sending relay, the backup target and the update helper; an admin who wants a copy of the mail has
  easier ways. It is not something a person without the admin role can reach.
- **Bringing an MX or SPF record "in line" can remove things.** The Cloudflare panel can rewrite a
  record that works but is not written the way UwUMail would write it. For MX that means our value
  becomes the only one at that name — a second, backup MX would go — and for SPF it means other
  senders listed there fall away. The tick is off by default, sits under its own heading, and both
  the panel and [deployment.md](deployment.md) say so in plain words.
