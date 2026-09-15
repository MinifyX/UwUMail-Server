# Security audit — before going public

September 2026, at the point where the code and the container image became public. Scope: the
whole server at that state — the SMTP core (`uwumail-smtp`), JMAP (`uwumail-jmap`), the web portal
with logins, two-factor authentication and forwarding (`uwumail-web`), storage (`uwumail-store`),
and the container image and its build.

This was done with Claude, not by an independent security firm. It is an honest sweep, not a
certificate. To report something new, see [SECURITY.md](../SECURITY.md).

## Summary

| Severity | Found | Fixed | Accepted / planned |
| --- | --- | --- | --- |
| Medium | 2 | 2 | 0 |
| Hardening | — | applied | — |

Both findings are fixed. The rest of this file lists what was checked, what was hardened, and the
limits that are known and accepted for now.

## Threat model

The main attacker is **anyone who can send the server a mail or reach its web port**: they choose
the envelope, the headers, the body and the login they try. Secondary attackers are **an
authenticated user** abusing their own account to reach others, **the network** (open Wi-Fi, a
hostile resolver), and **the container host** after a break-in through the server process. What an
attacker wants: to relay mail, to send or receive as someone else, another person's mail, a login,
or code execution on the host.

## Findings

### M1 — A spoofed Sender or a hidden second From on submission (fixed)

Submission only checked the one `From` address the parser returns, which is the **last** `From`
header. A message with two `From` headers passed the check on the last one while it was delivered
and displayed with the first, and a `Sender` header was not checked at all. An authenticated user
could therefore send mail that shows as another person on the same domain, with DKIM and DMARC
aligned. Fixed: a message must now have exactly one `From` header, and every `From` and `Sender`
address has to belong to the account (`claimed_addresses` in `crates/uwumail-smtp/src/submission.rs`,
with tests, and confirmed against a running server).

### M2 — Forwarding confirmation mails could be sent to anyone, repeatedly (fixed)

Adding an external forwarding target sends a confirmation mail to that address. A person is limited
to five targets, but by deleting and re-adding they could make the server send confirmation mails to
any address as often as they liked — a way to harass a third party and to burn the server's sending
reputation. Fixed with a throttle: one confirmation per address per hour, and a per-account cap
per hour and per day, recorded in the person's own activity list.

## What was checked and held up

- **Relaying.** Port 25 refuses mail to addresses that are not local; submission requires a login
  and only lets a person send from their own addresses. Address tricks (source routes, quoted
  locals, `%`-hacks, trailing dots, IP literals) do not open a relay.
- **SMTP smuggling and STARTTLS injection.** Bare `<LF>.<CR><LF>` and friends do not end the data
  early; anything pipelined before a STARTTLS handshake is dropped.
- **Brute force.** Web, JMAP and SMTP logins throttle per network; the second-factor step limits
  attempts per login and per network, so TOTP and recovery codes cannot be guessed.
- **Two-factor and passkeys.** The WebAuthn verifier checks the origin exactly, the RP-ID hash, the
  challenge (constant time), user presence and the signature, and rejects a signature counter that
  goes backwards. TOTP follows RFC 6238 with a one-step window and a stored last-step, so a code
  works once. Enabling a second factor makes mail apps require app passwords, and the JMAP login
  cache is invalidated the moment credentials change.
- **Sessions and CSRF.** Session tokens are random and stored only as a hash; the cookie is
  `__Host-`, `Secure`, `HttpOnly`, `SameSite=Strict` over HTTPS; state-changing API calls need the
  session's CSRF token; sensitive account changes need the password again unless it was just given.
- **Access control.** Admin endpoints require an admin session; a person only ever reaches their own
  account's data. The web app is served from an in-memory allow-list, so path traversal finds
  nothing on disk.
- **Forwarding and SRS.** Junk and quarantined mail is never forwarded; a loop check stops mail
  bouncing between servers; external forwarding is off unless enabled and can be blocked per person.
  SRS return paths are an HMAC-SHA256 over the day, domain and local part with a 32-byte secret,
  compared in constant time and valid for 21 days, and are only accepted on port 25 with an empty
  sender — so they cannot be forged into a relay.
- **Headers.** The server strips `Authentication-Results` that claim to be from itself, so a sender
  cannot fake the server's own verdicts.

## Container hardening

- **Distroless runtime** (`gcr.io/distroless/cc-debian13`, pinned by digest): glibc,
  ca-certificates and tzdata, but no shell and no package manager, so a break-in in the server
  process has almost no tools to build on. About 69 MB.
- **Unprivileged and read-only.** The server runs as uid 10001 on a read-only root file system;
  only the data volume and a small `/tmp` are writable. The compose files drop every Linux
  capability and add back only `NET_BIND_SERVICE`, which the binary needs for the low ports.
- **Scanned continuously.** CI scans the image with Trivy before it is pushed and fails on a fixable
  High or Critical; a weekly workflow re-scans the published image; a `cargo audit` job checks the
  Rust dependencies. All GitHub Actions are pinned to commit hashes, and a gitleaks workflow keeps
  secrets out of the public repository.

## Known limits (accepted for now)

- **HSTS only with a real certificate.** The server's own HTTPS listener sends
  `Strict-Transport-Security: max-age=31536000` once it has a certificate that is not self-signed
  (no `includeSubDomains`, no preload). Behind a reverse proxy, the proxy decides.
- **The setup code** (about 59 bits) is printed to the log while no admin exists. Wrong guesses
  count towards the login throttle per IP; the code stops working as soon as an admin exists.
- **App passwords** are 16 characters from a 31-letter alphabet (~79 bits), stored as a SHA-256
  hash. Guessing them online is infeasible and the throttles apply; they are deliberately not
  argon2-hashed so mail apps stay fast.
- **One account per login.** The server has no shared-mailbox model yet; JMAP exposes exactly the
  logged-in account.
- **No image signing yet.** The published image is not signed (e.g. cosign); it is pinned by digest
  where it is used and scanned in CI.
