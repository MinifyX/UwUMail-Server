# Architecture

```
                         ┌──────────────── uwumail-server (one process) ───────────────┐
 other mail servers ───▶ │ :25   SMTP (MX)  ──┐                                          │
 mail apps          ───▶ │ :587  submission ──┼─▶ uwumail-smtp ──▶ uwumail-store ──▶ /data│
                    ───▶ │ :465  submission ──┘      │  ▲             SQLite + blobs     │
                         │                           ▼  │                                │
 other mail servers ◀─── │                    delivery queue                            │
                         │                                                              │
 browsers, JMAP     ───▶ │ :443  HTTPS  (health, soon JMAP, admin, web mail)            │
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
- Private keys and the ACME account are written with mode 0600; the container
  runs as an unprivileged user with only `CAP_NET_BIND_SERVICE`.
