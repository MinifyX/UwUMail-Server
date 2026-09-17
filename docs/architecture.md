# Architecture

```
                         ┌──────────────── uwumail-server (one process) ───────────────┐
 other mail servers ───▶ │ :25   SMTP (MX)  ──┐                                          │
 mail apps          ───▶ │ :587  submission ──┼─▶ uwumail-smtp ──▶ uwumail-store ──▶ /data│
                    ───▶ │ :465  submission ──┘      │  ▲             SQLite + blobs     │
                         │                           ▼  │                                │
 other mail servers ◀─── │                    delivery queue                            │
                         │                                                              │
 browsers, JMAP     ───▶ │ :443  HTTPS  JMAP, web portal (uwumail-web), health          │
 Let's Encrypt      ───▶ │ :80   ACME challenges, redirect to HTTPS                     │
                         └──────────────────────────────────────────────────────────────┘
```

## Crates

### `uwumail-store`

Everything persistent, under one data directory:

| Path | Content |
| --- | --- |
| `uwumail.db` | SQLite in WAL mode: settings, domains, DKIM keys, accounts, addresses, mailboxes, message metadata, threads, keywords, change log, full-text index, outbound queue |
| `blobs/ab/cd/<sha256>` | Raw messages, stored once per content and reference-counted by triggers |
| `tls/` | ACME account, certificates, self-signed fallback |

All methods are async and run SQLite on the blocking pool: one writer
connection (immediate transactions) and a small pool of read-only
connections.

Design choices that matter later:

- **Change log per account.** Every write bumps the account's `modseq` and
  records `(kind, id, created|updated|destroyed)`. JMAP `*/changes` and IMAP
  CONDSTORE/QRESYNC read from it; a broadcast channel wakes push connections.
- **IMAP UIDs from day one.** Mailbox membership stores the UID per mailbox,
  with `uid_validity` and `uid_next`, so IMAP needs no second index.
- **Threads by message id.** Every referenced message id is mapped to a thread,
  so replies that arrive before their parent still join the conversation.

### `uwumail-smtp`

- `inbound`: one session type for MX and submission. Uses `smtp-proto` for
  command and DATA/BDAT parsing (including SMTP smuggling protection).
- `checks`: SPF, DKIM and DMARC through `mail-auth`, with a TTL cache that
  tests pre-fill.
- `spam`: scores mail from other servers (authentication results, greeting,
  reverse name, blocklists, reputation) and decides between inbox,
  greylisting, Junk and refusing; see [spam-filter.md](spam-filter.md). Lookups
  are bounded in time and a question without an answer scores nothing. The
  message itself is read once on a blocking thread (`spam::content` with
  `html`, `links` and `attachments`) for phishing links, dangerous attachments
  and header oddities; link domains are asked about on Spamhaus DBL. Moving mail
  into or out of Junk moves the sender's count in the store. `spam::bayes` takes
  messages apart into tokens, hashed with a key of the server, and learns them
  from a queue in the store in the background.
- `sender_lists`: allowed and blocked senders of a person, a domain and the
  server, decided per recipient before the score. Host names count only when
  the reverse name points back to the sending address.
- `spam::words`, `spam::feeds`, `spam::lists`: word lists compiled into regex
  sets per scope, and the built-in lists, kept compiled between messages and
  compiled again when a list or its switch changed. `fetch` fetches lists from
  public addresses only (a resolver that drops private ones), without
  redirects, size-capped, zstd unpacked, with ETag / Last-Modified.
- `outbound` + `client`: the delivery worker claims due recipients with a
  lease, groups them by domain, resolves MX (or a relay / static route),
  delivers with opportunistic TLS, and records the outcome per recipient.
- `mta_sts` + `https`: our domains' policies, and the policies of domains we
  deliver to. Those are fetched over HTTPS with a valid certificate and cached
  until they expire; an enforced policy limits delivery to the MX hosts it lists
  and requires a certificate valid for the host.
- `reports`: DMARC aggregate and TLS reports addressed to `dmarc-reports@` and
  `tls-reports@` a hosted domain are unpacked (capped), checked to be about the
  domain and stored as numbers instead of landing in a mailbox.
- `dnscheck`: the records a domain needs (MX, SPF, DMARC, DKIM) and the
  recommended ones (TLS reporting, SRV, MTA-STS including the policy file),
  resolved from the root servers down.
- `dkim`: RSA-2048 and Ed25519 keys per domain; submitted mail is signed with both.
- `forward` + `srs`: after local delivery, mail also goes to a person's confirmed
  forwarding addresses. Mail to other servers gets an SRS envelope sender on
  the person's domain (HMAC-SHA256, valid 21 days), so SPF passes there; DKIM
  signatures stay intact. Suspicious mail (DMARC quarantine or a junk score) is never
  forwarded, and a `Delivered-To` header stops loops. Bounces to SRS addresses
  are only accepted with an empty sender and go back to the original sender.
- `dsn` + `texts`: bounces in German or English. Mail to our own people uses
  the internal tone (playful by default), mail to anyone else the external
  tone (neutral by default).

### `uwumail-jmap`

JMAP over HTTP with axum. Ids are a type letter plus the database id (`m12`,
`e34`, `t56`); blob ids are content hashes (`b<sha256>` for whole messages
and uploads, `p<sha256>_<part>` for single MIME parts). States are the
account's change sequence number, so every `/changes` call reads straight
from the store's change log, and push listens to the same broadcast channel.
`EmailSubmission/set` goes through `Smtp::submit`, exactly like SMTP
submission: sender checks, DKIM, local delivery and the queue.

### `uwumail-web`

The web portal: a JSON API under `/api` and the React app from `web/`
(Vite, Tailwind, i18next; Nyu and the design tokens come from the UwUMail
app). `build.rs` embeds `web/dist` into the binary, so the server stays one
file; a build without `web/dist` shows a simple landing page instead. The
app serves every page from one `index.html` and picks the page from the URL.

Everyone logs in at the same place and lands in "My account" (`/account`);
admins also get "Server" (`/admin`). Portal preferences (language, tone,
Simple/Pro, theme) are stored per account.

The server overview opens with a health check in five areas:

- **DNS:** the latest DNS check of each domain, TLS failures and our own mail
  failing DMARC according to last week's reports.
- **Certificate:** days left and whether the certificate matches the hostname.
- **Sending:** failed logins or connections from the queue worker, a delivery
  probe, stuck mail and a high share of bounces.
- **Storage:** free disk space in the data directory and mailboxes near their limit.
- **Login:** admins without a second factor.

DNS checks run every six hours. The delivery probe runs hourly for a relay and
every six hours for direct delivery, and only when no mail went out
successfully in that time. For a relay, the probe logs in and quits. For direct
delivery, it reads the greeting of Gmail's MX on port 25 and quits. It never
sends mail. Admins can run all checks at once with "Check now".

### `uwumail-server`

The binary: configuration (`figment`: TOML + `UWUMAIL_*` environment),
certificates (`instant-acme`, file reload, self-signed), HTTP(S) with `axum`,
listeners and graceful shutdown, and management commands. With a paired
gateway it runs the tunnel client: connections that arrive through the tunnel
go to the same SMTP sessions and HTTP apps as the local listeners, and the
delivery queue connects to other servers through the gateway.

### `uwumail-tunnel` and `uwumail-gateway`

The [UwUMail Gateway](gateway.md) is a separate program for a small VPS.
`uwumail-tunnel` holds what both sides share:

- **QUIC** (`quinn` on rustls and aws-lc-rs, TLS 1.3 only, ALPN
  `uwumail-tunnel/1`). The server dials out and keeps the connection alive with
  keep-alives every 10 seconds; after 30 silent seconds it counts as lost.
- **Identities:** a self-signed Ed25519 certificate per side, pinned by the
  SHA-256 of the certificate. The server trusts only the fingerprint from its
  pairing code; the gateway accepts any client certificate in the handshake and
  decides afterwards, by fingerprint, whether it is the paired server. Handshake
  signatures are always checked, so a side has to hold its key.
- **Pairing codes:** `uwugw1` + base32 of the gateway's addresses and tunnel
  port, its fingerprint, a 128-bit one-time token and a 4-byte checksum.
- **Streams:** one per carried connection, each starting with a JSON message
  behind a 4-byte length. The server's first stream is the control stream
  (`Hello` with the token while pairing, answered by `Welcome` or `Refused`).
  The gateway opens a stream per public connection (`Open`: service, client
  address). The server opens one per outgoing connection (`Connect`: address,
  answered by `Connected` or `Failed`).

`uwumail-gateway` keeps its key, the paired server and the current token in
its state directory, checks the pairing every two seconds (so `unpair` takes
effect while it runs), limits connections in total and per network, answers
`421` on the SMTP ports while no server is connected, and connects outwards
only to its mail ports on public addresses.

## Security notes

- Submission requires TLS before AUTH (configurable), limits login failures
  per network (/64 for IPv6), and checks both the envelope sender and every
  From address against the account's addresses.
- `Authentication-Results` headers that claim our host name are removed from
  incoming mail before we add our own.
- Received headers of submitted mail contain neither the client's IP address
  nor its HELO name (opt-in via `smtp.reveal_client_ip`).
- Logins from mail apps (JMAP, SMTP) go through one check. App passwords are
  16 random characters (about 79 bits), so a SHA-256 lookup is enough; they
  carry scopes ("mail", "smtp"), an optional expiry and when they were last
  used. The main password is checked with Argon2id and only accepted while the
  person has no second factor and has not asked for app passwords only. A
  refused main password shows up in the person's activity list.
- Second factors: authenticator apps (RFC 6238, SHA-1, 6 digits, ±30 s, each
  code once) and passkeys (WebAuthn without attestation; ES256, EdDSA, RS256,
  verified with aws-lc-rs). The first one brings ten recovery codes, stored as
  SHA-256. Password links replace the password, never the second factor.
  Sensitive changes (second factors, app passwords) need the password again
  unless the login or the last confirmation is younger than ten minutes.
- Changes to someone's login are written to their activity list and put into
  their inbox as a short notice, so a takeover does not go unnoticed.
- Web portal sessions: a random token in an `HttpOnly`, `SameSite=Strict`
  cookie (`__Host-` prefixed and `Secure` over HTTPS); only its SHA-256 is
  stored. Requests that change something need the session's CSRF token in
  `X-CSRF-Token`; logins share the per-network failure limit. The app is
  served with a strict Content-Security-Policy and `frame-ancestors 'none'`.
  Over the server's own HTTPS with a trusted certificate, responses carry
  `Strict-Transport-Security: max-age=31536000` (not with a self-signed one).
- Private keys and the ACME account are written with mode 0600; the container
  runs as an unprivileged user with only `CAP_NET_BIND_SERVICE`.
