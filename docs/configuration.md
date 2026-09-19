# Configuration

UwUMail Server reads an optional TOML file (`--config` or `UWUMAIL_CONFIG`)
and then environment variables starting with `UWUMAIL_`. Nested keys use a
double underscore, e.g. `UWUMAIL_TLS__MODE=files`. Everything has a default
except `hostname`.

A list in an environment variable needs square brackets, also for one entry:
`UWUMAIL_HTTP__TRUSTED_PROXIES=[192.0.2.51, 172.30.25.2]`. With a bare or an
empty value the server refuses to start. In a compose file, quote it
(`"[192.0.2.51, 172.30.25.2]"`), or YAML reads a list of its own and Compose
refuses the file.

The stock `compose.yaml` only passes on the variables listed under
`environment:`; anything else in `.env` never reaches the server, so add
further settings there. `UWUMAIL_GATEWAY_CODE` and the six `…_BIND` variables in
`.env` are variables of that compose file, not settings of the server: the first
is handed on as `UWUMAIL_GATEWAY__CODE` (the key `gateway.code`, the only
spelling the server itself knows), and the others pick the ports on the Docker
host, one per listener:

| In `.env` | Container port | |
| --- | --- | --- |
| `UWUMAIL_SMTP_BIND` | 25 | mail from other servers |
| `UWUMAIL_HTTP_BIND` | 80 | certificate challenges, redirect to HTTPS |
| `UWUMAIL_HTTPS_BIND` | 443 | portal, JMAP, apps |
| `UWUMAIL_SUBMISSIONS_BIND` | 465 | mail apps, TLS |
| `UWUMAIL_SUBMISSION_BIND` | 587 | mail apps, STARTTLS |
| `UWUMAIL_IMAPS_BIND` | 993 | mail apps, IMAP |

Each takes a port or an `address:port`, and each only moves the host side: what
arrives from outside keeps the number it always had. `install.sh` asks about
every one it finds taken, and `update.sh` moves one written into `compose.yaml`
by hand over here.

Check a configuration with `uwumail-server check-config`.

## Settings in the admin panel

Admins can change sending, receiving, mail-app and tone settings under
*Einstellungen* / *Settings* in the web portal, and the spam filter under
*Spamfilter* / *Spam filter*. The server checks them and
applies them at once, without a restart. They are stored in the database
(`config.overlay`), so they survive updates.

The order, from weakest to strongest:

1. built-in defaults
2. settings from the admin panel
3. the config file
4. `UWUMAIL_*` environment variables

Anything the config file or the environment sets is shown as locked in the
panel. Remove it there to manage it from the panel instead. Listeners, TLS,
the data directory and logging stay file-only because changing them needs a
restart.

The same settings, with the same checks and the same order, are reachable from
the terminal — which is where the installer sets them, before there is a portal
to log into:

```bash
docker compose exec uwumail uwumail-server settings list spam
docker compose exec uwumail uwumail-server settings get spam.antivirus.enabled
docker compose exec uwumail uwumail-server settings set spam.antivirus.enabled true
docker compose exec uwumail uwumail-server settings unset spam.antivirus.enabled
```

`list` takes a prefix and says where each value comes from (default, set here,
config file). A setting the config file or the environment already fixes is
refused, the way the panel greys it out. Passwords are written with `-` as the
value and read from standard input, so they stay out of the shell history:

```bash
printf '%s' "$KEY" | docker compose exec -T uwumail uwumail-server settings set spam.feeds.abuse_ch_key -
```

A running server keeps the settings it started with, so a change made in the
terminal reaches it when it next starts. With the server stopped, the same
commands work through `docker compose run --rm uwumail …`.

The relay password is stored in the database like the rest (the portal never
shows it again). If you would rather keep it out of the database, set
`UWUMAIL_DELIVERY__RELAY__PASSWORD` in the environment.

## Accounts: people and services

*Server → Accounts* holds both. A **person** signs in to the portal and may use
everything. A **service** is a mailbox that belongs to a program: a backup
script that sends a report, a shop that sends receipts, a monitoring job. It
never signs in to the portal, never has a password of its own, and gets in only
with app passwords, which an admin creates on its page.

Every account has five switches, under *Protocols*:

| Switch | What it opens |
| --- | --- |
| SMTP | Sending through this server |
| IMAP | Mail apps like Thunderbird or Apple Mail |
| JMAP | UwUMail and everything else that speaks JMAP |
| Calendars (CalDAV) | Calendars |
| Contacts (CardDAV) | Address books |

A switch that is off holds every password at the door, including an app
password that still says it may do this — the check happens at the login, not
at the password. A person has all five; a service starts with SMTP, IMAP and
JMAP, and calendars and contacts off.

**Without IMAP and JMAP an account has no mailbox at all.** Mail to its address
is then refused right at the door with a `550`, or, when *Send mail here
instead* names an address of this server, handed on to that one. Nothing is
stored under the service itself. That is the shape for a sender that only ever
sends: no mailbox to fill up, and an answer that lands somewhere a person reads.

Turning a person into a service keeps the mail and turns the password they had
into an app password that does not expire, so what already works keeps working;
their second factors, passkeys and open sessions go, since none of them has
anything left to sign in to. The way back is the same button, and afterwards the
account needs a new password or an invitation link.

The same from the terminal:

```bash
docker compose exec uwumail uwumail-server account add reports@example.com --service --name "Reports"
docker compose exec uwumail uwumail-server account protocols reports@example.com \
  --imap off --jmap off --redirect me@example.com
docker compose exec uwumail uwumail-server account service someone@example.com on
```

`account list` marks a service as such, and says `sends only` when it has no
mailbox. App passwords are made in the portal, on the account's page.

```toml
# Public name of the server. Used in SMTP greetings, MX records and the certificate.
hostname = "mail.example.com"
data_dir = "/data"

[listen]              # empty string = off
smtp = "[::]:25"
submission = "[::]:587"
submissions = "[::]:465"
imaps = "[::]:993"     # IMAP with TLS for mail apps
http = "[::]:80"       # ACME challenges and redirect to HTTPS
https = "[::]:443"
proxy = ""             # plain HTTP for a reverse proxy, e.g. "[::]:8080"

[tls]
mode = "acme"          # acme | files | self-signed
acme_email = ""
acme_directory = "https://acme-v02.api.letsencrypt.org/directory"
cert_file = ""         # files mode: PEM chain, reloaded when it changes
key_file = ""

[http]
trusted_proxies = []           # reverse proxies whose X-Forwarded-For/-Proto are believed, e.g. ["10.0.0.2"]

[smtp]
max_message_size = 52428800
max_recipients = 100
require_tls_for_auth = true
timeout_secs = 300
max_connections = 500
verify_senders = true         # SPF, DKIM, DMARC for incoming mail
trusted_relays = []           # servers in front that forward mail to us, e.g. ["10.0.0.5"]
enforce_dmarc_reject = true   # otherwise p=reject failures go to Junk
reveal_client_ip = false      # keep senders' IP and device name out of headers
allow_external_forwarding = true  # people may forward to other servers (after the address confirms); forwarding addresses set up by admins always may

[spam]                        # see spam-filter.md
enabled = true
blocklists = true             # ask Spamhaus ZEN, SpamCop and Barracuda
bayes = true                  # the learning filter
junk_score = 5.0
greylist_score = 2.0          # up to junk_score: suspicious senders retry once
greylist_delay_secs = 300
# reject_score = 15.0         # refuse from this score on; off unless set

[spam.feeds]                  # built-in lists, see spam-filter.md
urlhaus = true                # malware links (abuse.ch, needs abuse_ch_key)
malware_bazaar = true         # malware attachments (abuse.ch, needs abuse_ch_key)
bad_subjects = true           # spam subjects (mailcow)
disposable = true             # throwaway address domains (Rspamd)
freemail = true               # freemail providers (Rspamd)
redirectors = true            # link shorteners (Rspamd)
# abuse_ch_key = "..."        # from auth.abuse.ch; free for non-commercial use only

[spam.antivirus]              # ClamAV beside the server, see antivirus.md
enabled = false               # needs the scanner running: docker compose --profile antivirus up -d
address = "clamav:3310"        # where clamd listens
timeout_secs = 30             # after that the message goes on unchecked
max_size = 26214400           # bigger messages are not sent to the scanner (its own limit)

[delivery]
concurrency = 16
max_lifetime_hours = 120
connect_timeout_secs = 30
command_timeout_secs = 300
mx_port = 25
require_tls = false

# Send everything through another server, e.g. a VPS or a sending service.
# [delivery.relay]
# host = "smtp.example.net"
# port = 587
# security = "starttls"       # starttls | tls | none
# username = "me"
# password = "secret"

# Fixed destinations per domain, checked before the relay and DNS.
[delivery.routes]
# "internal.example" = "10.0.0.5:25"

# A UwUMail Gateway in front of a server at home, see docs/gateway.md.
[gateway]
code = ""              # the gateway's pairing code, used once; the pairing then lives in the database

[tone]
language = "de"        # de | en
internal = "playful"   # playful | neutral: mail to our own people
external = "neutral"   # neutral | light: mail to everyone else

[log]
format = "text"        # text | json
level = "info"
```
