# Security audit — UwUMail server and gateway, September 2026

A second security pass over the server, after [security-audit.md](security-audit.md), and the first
one to cover the gateway and tunnel. Scope: the SMTP core (`uwumail-smtp`), JMAP (`uwumail-jmap`),
the web portal and API (`uwumail-web`, `web/`), storage (`uwumail-store`), the server binary and
container (`uwumail-server`, `docker/`), and — in the `UwUMail-Server-gateway` worktree — the VPS
gateway (`uwumail-gateway`) and the QUIC tunnel with its pairing (`uwumail-tunnel`,
`deploy/gateway`).

Done with Claude, not an independent firm; an honest sweep, not a certificate. Step-by-step exploits
are left out; reproduction notes are in the private `security-test-notes.local.md` (gitignored). Many
findings below were confirmed against the running test instance (a Docker container of
`ghcr.io/minifyx/uwumail-server:edge`) using throwaway accounts on a `sectest.test` test domain,
created and removed over the CLI. No mail was sent outside, and no fuzzing, brute force or denial of
service was run against it.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 1 (S-4, added later) | 1 | 0 |
| Low | 2 | 2 | 0 |
| Informational | 1 | 1 | 0 |
| Gateway/Tunnel | 0 | — | 0 |

*Updated 16 September 2026:* S-1 to S-3 were fixed right after this report. S-4, a real gap in DMARC
enforcement, was found later while building the spam filter and fixed the same day; see the
[addendum](#addendum--16-september-2026).

The earlier findings **M1** (a spoofed `Sender` or hidden second `From` on submission) and **M2**
(forwarding-confirmation spam) still hold, and both were confirmed live. The "what held up" list
from the first audit was re-checked. The gateway and tunnel are new to this report and came out
clean, with the one known accepted risk (the VPS can present a false client IP) confirmed to go no
further than logs, SPF and rate-limits — no authentication bypass.

## Threat model

Unchanged for the server: the main attacker is **anyone who can send a mail or reach the web port**;
secondary are **an authenticated user** against others, **the network**, and **the container host**
after a break-in. For the gateway there is one more, and it is unusual: **the VPS is only partly
trusted**. The tunnel is built so that a compromised VPS can disrupt and observe metadata, but not
read mail or passwords (TLS ends at home) and not sign in as anyone.

## New findings

### S-1 — Forged `Authentication-Results` with a version number survive stripping *(Low, fixed in 46c0de0)*

| Field | Content |
| --- | --- |
| ID | S-1 |
| Severity | Low — CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:N/I:L/A:N |
| Component | `crates/uwumail-smtp/src/headers.rs` (`strip_forged_auth_results`) |
| Attacker & preconditions | Any mail sender |
| Impact | The server strips inbound `Authentication-Results` headers that claim to come from itself, so a sender can't fake the server's own verdicts. The check compares the whole portion before the first `;` to the hostname. RFC 8601 allows an optional version number after the authserv-id (`Authentication-Results: host 1; …`). A header like `Authentication-Results: <our-hostname> 1; dkim=pass header.d=trusted-bank.example` is therefore **not** stripped and reaches the mailbox. A downstream consumer that trusts an `Authentication-Results` line by matching the authserv-id could be misled into believing a fake verdict. |
| Evidence | **Verified live.** A mail delivered with both a plain and a version-suffixed forged header kept the version-suffixed one; the plain one was stripped. The server still prepends its own authoritative header at the very top, and UwUMail's own client does not consume `Authentication-Results`, so real-world impact is limited today. |
| Recommended fix | Parse the authserv-id as the first whitespace-delimited token of the pre-`;` portion and compare that (case-insensitively) to the hostname, ignoring an optional trailing version number. Strip on a match. |
| Regression test | Extend the `strips_only_our_auth_results` test with a `"<hostname> 1; …"` line and assert it is removed, while a genuinely different authserv-id is kept. |

### S-2 — Submission does not check `Resent-*` sender identity headers *(Low, fixed in ef17cb7)*

| Field | Content |
| --- | --- |
| ID | S-2 |
| Severity | Low — CVSS:3.1/AV:N/AC:L/PR:L/UI:R/S:U/C:N/I:L/A:N |
| Component | `crates/uwumail-smtp/src/submission.rs` (`claimed_addresses`) |
| Attacker & preconditions | An authenticated user of the server |
| Impact | M1 makes every `From` and `Sender` address belong to the account and refuses a hidden second `From`. It does not look at `Resent-From` / `Resent-Sender`, which some mail clients display. An authenticated user can send a message with `Resent-From: someone-else@ourdomain` that a receiving client might show as coming from that person. The `Sender` count is also not capped, so with two `Sender` headers the outcome depends on which one the parser returns (in testing, a `Sender` naming an unowned address was still refused). |
| Evidence | **Verified live.** Authenticated as one account, a message with `Resent-From:` naming a different local account was accepted and queued. A message with two `Sender` headers (owned + unowned) was refused. |
| Recommended fix | Extend the ownership check to `Resent-From` and `Resent-Sender` (treat like `From`/`Sender`), and refuse more than one `Sender`. Keep it defence-in-depth: the practical impact depends on the receiving client. |
| Regression test | A `claimed_addresses` test that returns `Resent-From`/`Resent-Sender` addresses for the same ownership check, and refuses a second `Sender`. |

### S-3 — Report-recipient RCPT reply ends in a bare LF *(Informational, fixed in 8bccb30)*

| Field | Content |
| --- | --- |
| ID | S-3 |
| Severity | Informational — no security impact |
| Component | `crates/uwumail-smtp/src/inbound.rs` (the `report_recipient` branch, ~line 692) |
| Attacker & preconditions | Any sender addressing `dmarc-reports@`/`tls-reports@` a local domain on port 25 |
| Impact | The `250 2.1.5 Recipient OK` reply for a report address is written with a bare `\n` instead of `\r\n`. RFC 5321 requires every reply line to end with CRLF. Well-behaved report senders tolerate it, but it is a protocol-conformance defect and inconsistent with every other reply. |
| Evidence | **Verified live.** The reply bytes for a report recipient end `4f 4b 0a` (`OK\n`), while all other replies end `0d 0a` (`\r\n`). |
| Recommended fix | Replace the multi-line string literal with `"250 2.1.5 Recipient OK\r\n"`. |
| Regression test | A small check that the reply for a report recipient ends in `\r\n`. |

## Gateway and tunnel

No findings. The design in [gateway.md](../docs/gateway.md) — "trust the VPS like the server" — is
reflected in the code:

- **Outbound is not an open proxy.** The gateway only connects where the config allows:
  `OutboundConfig::allows` requires a mail port (25/465/587) and `net::is_global(ip)`. `net::is_global`
  is thorough — it rejects loopback, private (10/8, 172.16/12, 192.168/16), link-local (including the
  `169.254.169.254` cloud-metadata address), carrier-grade NAT (100.64/10), benchmarking,
  documentation, reserved and the matching IPv6 ranges, and it canonicalises IPv4-mapped and NAT64
  addresses first. Verified by its unit tests.
- **Pairing.** The one-time token is 128 bits from the system RNG, compared in constant time, and
  redacted in logs. Once a server is paired, the saved pairing (its certificate fingerprint) takes
  precedence, so the token cannot be reused even if its removal fails. The pairing-code parser is
  bounds-checked and rejects trailing bytes.
- **Certificate pinning both ways.** The server pins the gateway's certificate fingerprint (from the
  pairing code); the gateway accepts any client certificate in the handshake but then only serves the
  paired server's fingerprint, refusing any other. Handshake signatures are verified on both sides, so
  a peer must hold the private key of the certificate it presents.
- **The tunnel frame parser** caps message length at 64 KB *before* allocating, so a hostile length
  can't exhaust memory; bodies are JSON via serde. The carried connection's bytes are left untouched.
- **TLS ends at home.** `pipe::pipe` is a plain bidirectional copy; the gateway never terminates TLS
  and never sees passwords or mail content.
- **A server-supplied host name** that goes into SMTP `421` answers is validated (`clean_hostname`),
  so it can't smuggle a CRLF and extra response lines.
- **systemd hardening** is comprehensive (dedicated user, only `CAP_NET_BIND_SERVICE`,
  `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp/Devices`, `MemoryDenyWriteExecute`,
  a `@system-service` syscall filter minus `@privileged`/`@resources`, `StateDirectory` 0700).
- **`deploy-gateway.sh`** takes only the operator's own `SSH` target; there is no command injection
  from untrusted input.

**Accepted risk confirmed:** the gateway forwards the real client IP to the home server in the
stream header, so a compromised VPS could present a false client IP. That only affects SPF, logging
and per-network rate-limits — authentication is end-to-end (TLS + credentials terminate at home), so
a forged IP cannot sign in as anyone. This matches the documented residual risk; nothing more was
reachable.

## Regression check of earlier findings

| ID / area | Topic | Status |
| --- | --- | --- |
| M1 | Spoofed `Sender` / hidden second `From` on submission | **holds — verified live** (From-spoof, double-From and Sender-spoof all refused with 550); see S-2 for the `Resent-*` gap |
| M2 | Forwarding-confirmation spam | holds — `check_confirmation_allowed` enforces one per address per hour and per-account hourly/daily caps |
| Relaying | Port 25 refuses non-local; submission needs login and own addresses | **holds — verified live** (external recipient → 550 relaying denied; local → 250); address tricks reviewed |
| SMTP smuggling / STARTTLS | Bare `<LF>.<CR><LF>`, pipelining before STARTTLS | holds — `smtp-proto` framing; STARTTLS resets HELO and transaction and drops pipelined bytes |
| Brute force | Web/JMAP/SMTP throttle per network; 2FA per login and network | holds — auth limiter per /64, MAX_AUTH_FAILURES, timed lockouts |
| 2FA / passkeys | WebAuthn origin/RP-ID/challenge/counter; TOTP one-step | holds (code review) |
| Sessions / CSRF | `__Host-`, `Secure`, `HttpOnly`, `SameSite=Strict`, CSRF token, re-auth window | holds (code review) |
| Access control | Admin routes need admin session; users reach only their own data | **holds — verified live** (cross-account blob download denied: 404; own download 200); JMAP `check_account` pins the accountId, store queries scope by `account_id`, `blob_accessible` scopes blob hashes by account |
| Forwarding / SRS | No forward of junk; loop check; SRS HMAC constant-time, 21-day window | holds — `Delivered-To` loop check, junk kept, SRS SHA-256 HMAC with constant-time compare |
| Headers | Strips `Authentication-Results` claiming to be us | holds for the plain form; **see S-1** for the version-suffixed bypass |
| Reports | DMARC/TLS parser (XML/zip/gzip), XXE, zip bombs | holds — parsing via `mail_auth` with a 20 MB unpack cap, in `spawn_blocking`, only for our domains, never stored in a mailbox |

## Container

The Dockerfiles passed Trivy's config scan (27/27 checks, no misconfigurations), `cargo audit` is
clean, and the hardening claims check out: distroless runtime, uid 10001, read-only root file system,
all capabilities dropped except `NET_BIND_SERVICE`. Image signing is still absent (a known limit).

## Known limits (unchanged)

HSTS only with a real certificate; the ~59-bit setup code printed while no admin exists; app
passwords are SHA-256 (~79 bits) for speed; one account per login; no image signing yet. None of
these changed.

## Priorities before 1.0 / before real mail

1. **S-1** — parse the authserv-id token so version-suffixed forgeries are stripped. Small change,
   worth doing before the DKIM/DMARC story is relied on downstream. **Done** (46c0de0).
2. **S-2** — extend the submission ownership check to `Resent-*` and cap `Sender`. **Done** (ef17cb7).
3. **S-3** — fix the bare-LF reply (trivial). **Done** (8bccb30).
4. Consider image signing (cosign) for the container.

## What could not be tested, and why

- **A real VPS / port 25 to the internet** — out of scope by the rules. The gateway was reviewed
  statically and against its unit tests; no live pairing against a real VPS was done.
- **Sending real mail outbound** — never done; all live tests used local recipients on a test domain.
- **Fuzzing** the MIME, SMTP, JMAP and tunnel parsers — not run in this pass (the parsers were read
  and, for the tunnel, exercised via their bounds tests); a `cargo fuzz` campaign is a good next step.
- **The production instance for the real domain** was left untouched apart from the explicitly
  authorised, non-invasive tests on a separate throwaway test domain, which was removed afterwards.

## Addendum — 16 September 2026

Found after the audit, while building the spam filter, by reading how `mail-auth` reports DMARC
alignment. It is the most serious finding in this report.

### S-4 — DMARC `p=reject` and `p=quarantine` were not enforced for plain forgeries *(Medium, fixed in 9a01c9a)*

| Field | Content |
| --- | --- |
| ID | S-4 |
| Severity | Medium — CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:N/I:H/A:N |
| Component | `crates/uwumail-smtp/src/checks.rs` (`verify`) |
| Attacker & preconditions | Any mail sender; the forged domain publishes DMARC with `p=reject` or `p=quarantine` |
| Impact | Incoming mail that fails the sender's `p=reject` policy is meant to be refused (unless `smtp.enforce_dmarc_reject` is off), and `p=quarantine` puts it into Junk. The check required *both* alignment results to be `Fail`. `mail-auth` 0.13.2 (following RFC 9989) only reports `Fail` for a mechanism that passed for another domain; a plain forgery, whose SPF fails and which carries no valid DKIM signature, gets `None` for both. So a forgery in the name of a bank that publishes `p=reject` was accepted into the inbox, although the server's own `Authentication-Results` said `dmarc=fail … policy.dmarc=reject`. Mail that passed DKIM, which is most legitimate mail, was never affected. |
| Evidence | **Verified live** through the gateway from a public address: a forgery in the name of `example.com` (`v=spf1 -all`, `p=reject`) to a throwaway test domain went to the inbox before the fix and was refused with `550 5.7.1` after it; the test domain was removed afterwards. Two end-to-end tests forge mail from a reject and a quarantine domain, and both failed before the fix. |
| Fix | Use the overall DMARC result (`DmarcOutput::result()`), which fails whenever a policy is published and nothing passed aligned with it. |
| Regression test | `forged_mail_from_a_domain_that_rejects_it_is_refused` and `forged_mail_from_a_domain_that_quarantines_it_goes_to_junk` in `crates/uwumail-smtp/tests/flow.rs` |

Enforcement is now real, so the sending address the server sees has to be right: behind something
that hides it, forwarded mail without DKIM from `p=reject` domains would be refused. Behind a server
in `smtp.trusted_relays` and behind the UwUMail Gateway the real address arrives; both were checked.

S-3 also has its regression test now: the reply to a report address in
`reports_are_read_by_the_server_instead_of_landing_in_a_mailbox` has to end with CRLF.

Since this report, the gateway runs on a real VPS, and outbound mail leaves through it; the live
check for S-4 went that way.
