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
- `outbound` + `client`: the delivery worker claims due recipients with a
  lease, groups them by domain, resolves MX (or a relay / static route),
  delivers with opportunistic TLS, and records the outcome per recipient.
- `dkim`: RSA-2048 and Ed25519 keys per domain; submitted mail is signed with both.
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

The server overview opens with a health check in four areas:

- **DNS:** the latest DNS check of each domain.
- **Certificate:** days left and whether the certificate matches the hostname.
- **Sending:** failed logins or connections from the queue worker, a delivery
  probe, stuck mail and a high share of bounces.
- **Storage:** free disk space in the data directory and mailboxes near their limit.

DNS checks run every six hours. The delivery probe runs hourly for a relay and
every six hours for direct delivery, and only when no mail went out
successfully in that time. For a relay, the probe logs in and quits. For direct
delivery, it reads the greeting of Gmail's MX on port 25 and quits. It never
sends mail. Admins can run all checks at once with "Check now".

### `uwumail-server`

The binary: configuration (`figment`: TOML + `UWUMAIL_*` environment),
certificates (`instant-acme`, file reload, self-signed), HTTP(S) with `axum`,
listeners and graceful shutdown, and management commands.

## Security notes

- Submission requires TLS before AUTH (configurable), limits login failures
  per network (/64 for IPv6), and checks both the envelope sender and every
  From address against the account's addresses.
- `Authentication-Results` headers that claim our host name are removed from
  incoming mail before we add our own.
- Received headers of submitted mail contain neither the client's IP address
  nor its HELO name (opt-in via `smtp.reveal_client_ip`).
- Web portal sessions: a random token in an `HttpOnly`, `SameSite=Strict`
  cookie (`__Host-` prefixed and `Secure` over HTTPS); only its SHA-256 is
  stored. Requests that change something need the session's CSRF token in
  `X-CSRF-Token`; logins share the per-network failure limit. The app is
  served with a strict Content-Security-Policy and `frame-ancestors 'none'`.
- Private keys and the ACME account are written with mode 0600; the container
  runs as an unprivileged user with only `CAP_NET_BIND_SERVICE`.
