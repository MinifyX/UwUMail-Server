# Prometheus metrics

For people who run a monitoring system anyway, the server can answer
`GET /metrics` in the [Prometheus text format](https://prometheus.io/docs/instrumenting/exposition_formats/):
how many accounts and domains there are, how full the disk and the queue are,
what the statistics counted since the start, how each health area is doing, and
which version runs for how long. Nothing in it names a person or an address.

It is off until an admin switches it on, and then it never answers everyone.

## Switching it on

Under *Server → Statistics → Prometheus metrics*:

- **Serve metrics** switches `/metrics` on. While it is off, the address answers
  `404` like any address that does not exist.
- **Token**: *Make a token* creates 32 random characters in the browser. Copy it
  before saving; the server keeps it, but never shows it again. A scraper sends
  it as `Authorization: Bearer <token>`. The server compares it in constant time,
  answers a wrong one with `401`, and counts wrong tokens like wrong passwords: a
  network that keeps guessing gets `429` for a while, even with the right token.
- **Only from these networks**: addresses or networks, one per line
  (`10.0.0.0/8`, `2001:db8::/32`, `192.0.2.7`). A scraper from anywhere else gets
  `403`, token or not. Behind a reverse proxy the address is the one the proxy
  forwarded, as everywhere else in the server (`http.trusted_proxies`).

With both, a scraper needs the token *and* one of the networks. A network alone
is enough for a scraper on a network you trust (the monitoring container next to
the server). The server refuses to switch the metrics on with neither, and a
token shorter than 16 characters.

The same three settings live in the config file or the environment, where they
show as locked in the portal:

```toml
[metrics]
enabled = true
token = "a-long-random-token-from-somewhere"
allowed_networks = ["10.0.0.0/8"]
```

```bash
UWUMAIL_METRICS__ENABLED=true
UWUMAIL_METRICS__TOKEN=a-long-random-token-from-somewhere
UWUMAIL_METRICS__ALLOWED_NETWORKS='[10.0.0.0/8]'
```

## Scraping

```yaml
scrape_configs:
  - job_name: uwumail
    scheme: https
    metrics_path: /metrics
    authorization:
      type: Bearer
      credentials: a-long-random-token-from-somewhere
    static_configs:
      - targets: ["mail.example.org"]
```

Every scrape reads a handful of numbers from the database and looks at the
health overview (at most every 30 seconds), so a scrape interval of 30 seconds
to a few minutes is plenty. The answer is never cached (`Cache-Control: no-store`).

## What there is

| Metric | Type | Labels | What it is |
| --- | --- | --- | --- |
| `uwumail_build_info` | gauge | `version` | Always 1; the version that runs |
| `uwumail_uptime_seconds` | gauge | | Seconds since the server started |
| `uwumail_accounts` | gauge | | Accounts, not counting those in the trash |
| `uwumail_domains` | gauge | | Domains |
| `uwumail_aliases` | gauge | | Additional addresses of accounts |
| `uwumail_storage_used_bytes` | gauge | | Mail stored in all mailboxes |
| `uwumail_data_disk_free_bytes` | gauge | | Free space on the disk of the data directory |
| `uwumail_data_disk_size_bytes` | gauge | | Size of that disk |
| `uwumail_queue_messages` | gauge | | Messages waiting to go to other servers |
| `uwumail_queue_recipients` | gauge | `state`: `pending`, `deferred` | Recipients in the queue: not tried yet, or waiting for another attempt |
| `uwumail_mail_received_total` | counter | | Mail from other servers or fetched mailboxes that reached someone |
| `uwumail_mail_junk_total` | counter | | Of those, filed as Junk for someone |
| `uwumail_mail_refused_total` | counter | `reason`: `unknown_recipient`, `spam`, `virus`, `policy`, `greylisted` | Mail or recipients refused at the door |
| `uwumail_mail_submitted_total` | counter | | Mail our own people sent |
| `uwumail_mail_delivered_total` | counter | | Recipients other servers took |
| `uwumail_mail_deferred_total` | counter | | Delivery attempts that will be tried again |
| `uwumail_mail_bounced_total` | counter | | Recipients given up on |
| `uwumail_login_failures_total` | counter | `protocol`: `smtp`, `imap`, `jmap`, `dav`, `managesieve`, `portal`, `other` | Failed logins |
| `uwumail_health_level` | gauge | `area` | Each area of the health overview: 0 fine, 1 warning, 2 problem, -1 not checked yet |
| `uwumail_alerts_open` | gauge | `level`: `info`, `warning`, `problem` | Open [admin alerts](admin-alerts.md) |
| `uwumail_backup_last_success_timestamp_seconds` | gauge | | When the last backup worked; only with backups set up |
| `uwumail_certificate_expiry_timestamp_seconds` | gauge | | When the certificate in use expires |

The counters are the same numbers as the statistics in the portal, counted in
memory since the server started: they begin at 0 after every restart, which
`rate()` and `increase()` understand. `policy` covers DMARC rejections, blocked
senders and relaying attempts. Greylisted mail is asked to come back later and
mostly counts as received once it does.

Areas of `uwumail_health_level` only appear when the server has them: `gateway`
with a paired UwUMail Gateway, `antivirus` with the virus scanner switched on.

## Alert rules to start with

```yaml
groups:
  - name: uwumail
    rules:
      - alert: UwUMailHealth
        expr: uwumail_health_level == 2
        for: 15m
      - alert: UwUMailQueueGrowing
        expr: uwumail_queue_recipients{state="deferred"} > 20
        for: 1h
      - alert: UwUMailCertificateExpiring
        expr: uwumail_certificate_expiry_timestamp_seconds - time() < 7 * 86400
      - alert: UwUMailBackupOld
        expr: time() - uwumail_backup_last_success_timestamp_seconds > 2 * 86400
      - alert: UwUMailLoginsFailing
        expr: sum(rate(uwumail_login_failures_total[15m])) * 60 > 10
        for: 15m
```

The server has its own [alerts](admin-alerts.md) that write to the admins as
well; Prometheus is for when you want everything in one place.
