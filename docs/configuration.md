# Configuration

UwUMail Server reads an optional TOML file (`--config` or `UWUMAIL_CONFIG`)
and then environment variables starting with `UWUMAIL_`. Nested keys use a
double underscore, e.g. `UWUMAIL_TLS__MODE=files`. Everything has a default
except `hostname`.

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

The relay password is stored in the database like the rest (the portal never
shows it again). If you would rather keep it out of the database, set
`UWUMAIL_DELIVERY__RELAY__PASSWORD` in the environment.

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
