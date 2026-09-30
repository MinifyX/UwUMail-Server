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

Admins can change sending, receiving, mail-app, tone and branding settings under
*Einstellungen* / *Settings* in the web portal ([branding.md](branding.md) explains the logo,
name, colour and languages), and the spam filter under
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
the data directory and the log format and level stay file-only because changing
them needs a restart; sending the log to Grafana Loki is in the panel, see below.

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
`UWUMAIL_DELIVERY__RELAY__PASSWORD` in the environment. The same goes for the
Google client secret of fetched mailboxes (`fetch.oauth.google_client_secret`,
*Einstellungen → Anmeldung*, see [fetch.md](fetch.md#microsoft-and-google)):
`UWUMAIL_FETCH__OAUTH__GOOGLE_CLIENT_SECRET`.

## Sending the log to Grafana Loki

The server can send its log lines to a [Grafana Loki](https://grafana.com/oss/loki/)
itself, so no Alloy or Promtail has to run next to it. With a UwUMail Gateway,
the gateway's lines come along: it hands them to the server through the tunnel,
and they go on with the label `source=gateway`. They also show up on the
portal's *Logs* page, marked *Gateway*.

Switch it on under *Server → Logs → Log shipping*. The address is Loki's
base address (`http://192.168.1.20:3100`, `https://loki.example.net`);
`/loki/api/v1/push` is added when it has no path of its own, so an address
behind a reverse proxy can name the whole path instead. Loki can be reached
without a login, with a username and password (Grafana Cloud: the user id and an
access token), or with a bearer token. *Send a test line* tries the address with
what is typed, before anything is saved or switched on.

**Log lines contain personal data** — login names and addresses, the IP
addresses of mail apps and other servers, the senders of received mail — and
sending them hands that data to another machine. Switching it on therefore
needs the admin to agree to exactly that (`privacy_consent`); the change log
notes who did. Switching it off in the portal takes the agreement back.

Every line carries the labels `app="uwumail"`, `instance` (the host name),
`source` (`server` or `gateway`) and `level`, plus any extra labels you give.
The line itself is the same JSON the server writes with `log.format = "json"`,
so one set of queries works for both ways into Loki:

```logql
{app="uwumail", level=~"warn|error"}
{app="uwumail", source="server"} | json | fields_message=~"failed.*login"
```

Lines wait in memory (up to 10'000) while Loki is away and go out once it is
back; past that the oldest are dropped, and the panel shows how many. Mail never
waits for Loki. The server's own `log.level` comes first: what the server does
not log, it cannot send.

```toml
[log.loki]
enabled = false
privacy_consent = false  # needed for enabled: log lines contain personal data
url = ""                 # e.g. "http://192.168.1.20:3100"
username = ""            # basic authentication, or
password = ""
token = ""               # a bearer token instead
tenant = ""              # X-Scope-OrgID, for a Loki with several tenants
labels = []              # e.g. ["env=production"]
level = "info"           # error | warn | info | debug
gateway = true           # send the gateway's lines too
```

As environment variables: `UWUMAIL_LOG__LOKI__ENABLED=true`,
`UWUMAIL_LOG__LOKI__PRIVACY_CONSENT=true`, `UWUMAIL_LOG__LOKI__URL=…`, and so on.
The password and the token are stored in the database like the relay password;
set them in the environment to keep them out of it.

## Remote pictures through a VPN

A picture in a message that is loaded from the sender's server tells the sender
that the message was opened, when, and from which address. The webmail and the
UwUMail apps therefore don't load such pictures themselves: they ask the server
for them (`/jmap/image`, see [jmap-remote.md](jmap-remote.md)), and only once
the reader said they may be shown. The sender then sees the server, never the
reader.

With `egress.proxy` set, the server fetches them through a proxy too, so the
sender sees a VPN instead of the server. Two more kinds of request can take the
same way, each switched on by itself: the check for new UwUMail versions
(`egress.updates`, so GitHub does not learn where the server is) and fetching
from other providers: mail from their mailboxes, calendars people subscribed to
and calendars and contacts moved over ([calendar-import.md](calendar-import.md))
(`egress.fetch`; some providers refuse VPN addresses), and the AI assistant's requests to OpenAI, Anthropic and
the other providers on the internet (`egress.assist`, see [llm.md](llm.md); providers in the local network
are always reached directly). Pictures (`egress.pictures`) take it unless switched off,
and one-click unsubscriptions ([jmap-unsubscribe.md](jmap-unsubscribe.md)) go the way pictures go. DNS,
delivering mail, blocklists and list updates keep leaving directly. Outgoing
mail on port 25 could not go through a VPN anyway; providers block it, and
their addresses are on every blocklist.

**In the portal:** *Server → Settings → VPN & proxy* sets all of it while the server runs.
Pick a provider (NordVPN, Mullvad, Proton VPN, Surfshark, IVPN, AirVPN,
Windscribe and every other provider gluetun knows, or your own WireGuard or
OpenVPN server), paste the key or read the provider's WireGuard `.conf` or
`.ovpn` file, choose countries or cities, and press *Save and connect*. With the
machine's helper (`deploy/host`, version 2 or later) the portal writes `.env.vpn`,
adds `vpn` to `COMPOSE_PROFILES` in `.env` and starts gluetun, then points the
way out at `http://gluetun:8888`; *Switch the VPN off* stops it and lets
everything go straight again, keeping its settings for the next time. *Remove
VPN* takes it out entirely: the container, `.env.vpn`, the OpenVPN file and the
keys stored in the portal (helper version 3; an older one only stops the
container). Without the helper the portal shows `.env.vpn` and the command to
start it. The settings, keys included, are kept in the
server's database and never sent back to the browser. The helper takes only
gluetun's own variables, only values without quotes or line breaks, and an
`.ovpn` file only when every line is a directive of a plain connection (`client`,
`remote`, `proto`, `cipher`, `verb` and about eighty more), `auth-user-pass`
without a file, a comment, or an inline block with keys and certificates
(`<ca>`, `<cert>`, `<key>`, `<tls-auth>`, `<tls-crypt>`, …). Anything else,
such as `up`, `plugin` or a certificate read from a file, is refused, and so is
anything the list does not know: providers' files work, and one that starts
programs or reads files does not.

Two kinds of proxy work:

- `http://host:port`, a proxy that tunnels with `CONNECT`. The easiest is
  [gluetun](https://github.com/qdm12/gluetun) next to the server, which speaks
  OpenVPN and WireGuard and knows NordVPN, Mullvad, ProtonVPN and many other
  providers. `compose.yaml` has it behind the `vpn` profile; fill in the
  `GLUETUN_*` lines in `.env` and start it with
  `docker compose --profile vpn up -d`.
- `socks5://host:port`, for example a SOCKS5 proxy a VPN provider runs.

Both may carry a login (`http://user:password@host:port`); characters like `@`
in it are written percent-encoded (`%40`). The server resolves names itself and
hands the proxy an address, which it checked is on the open internet, so no
picture can reach a machine inside the network through the proxy.

`egress.fallback` decides what happens while the proxy can't be reached or
refuses the tunnel: `block` (the default) waits until it is back (no pictures,
no update check, no fetching), `direct` goes out from the server as if no proxy
were set, and the other side sees the server for that time.

When the proxy fails as a whole (its name does not resolve, nothing listens,
it turns the login down, or it refuses tunnels to many different hosts in a
row, as gluetun does while its VPN is down), it rests for 30 seconds: requests
meanwhile go straight or stay away as `fallback` says, without waiting for the
proxy first, and the log says so once instead of once per picture. Then one
request tries it again. A tunnel refused for one dead host alone never counts.
*Test the way out* in the portal always tries the proxy.

The server keeps the remote pictures it fetched for up to 7 days, shared by
everyone who reads the same message, in `cache/images` of the data directory
(left out of backups). `egress.image_cache_mb` (default 1024) is the most it
may take on disk; when it is full the pictures asked for least lately go first,
and `0` keeps nothing. How long a picture may take and how many each person
fetches at a time is in [jmap-remote.md](jmap-remote.md#patience).

```toml
[egress]
proxy = "http://gluetun:8888"
fallback = "block"
pictures = true
updates = false
fetch = false
image_cache_mb = 1024
assist = false
```

As environment variables: `UWUMAIL_EGRESS__PROXY`, `UWUMAIL_EGRESS__FALLBACK`,
`UWUMAIL_EGRESS__PICTURES`, `UWUMAIL_EGRESS__UPDATES`, `UWUMAIL_EGRESS__FETCH`,
`UWUMAIL_EGRESS__IMAGE_CACHE_MB` and `UWUMAIL_EGRESS__ASSIST`.
What the config file or a non-empty variable sets is locked in the portal; the
empty `UWUMAIL_EGRESS_PROXY=` and `UWUMAIL_EGRESS_FALLBACK=` that `compose.yaml`
passes on leave them to the portal. A proxy login belongs in `.env` or the
portal, not in a file anyone else reads.

## Text in pictures (OCR)

The server can read the text in a message's pictures, so the webmail finds a
date on a poster or an invitation that is only a picture
([jmap-image-text.md](jmap-image-text.md)). It runs
[Tesseract](https://github.com/tesseract-ocr/tesseract) for that, a program of
its own, one picture at a time and at most two at once, each for at most 20
seconds. The Docker image brings it along with German and English; installed
another way, install `tesseract-ocr`, `tesseract-ocr-deu` and
`tesseract-ocr-eng` (Debian and Ubuntu). Without it nothing breaks: the server
says OCR is unavailable and the webmail does without.

```toml
[ocr]
enabled = true
command = "tesseract"
languages = "deu+eng"
```

As environment variables: `UWUMAIL_OCR__ENABLED`, `UWUMAIL_OCR__COMMAND` and
`UWUMAIL_OCR__LANGUAGES`.

## Prometheus metrics

`GET /metrics` answers in the Prometheus format once it is switched on, under
*Server → Statistics* or in the `[metrics]` section (`enabled`, `token`,
`allowed_networks`). It stays off by default and never answers without a token
or an allowed network. [metrics.md](metrics.md) lists what it serves and how to
scrape it.

## Limits on the shape of a message

Besides its size (`smtp.max_message_size`), a message has to keep to a few
limits on its structure. They are fixed, and far above what mail programs
write:

| Limit | |
| --- | --- |
| Nesting | 64 levels of parts inside parts (a multipart part or a forwarded message counts as one level each) |
| Parts | 5,000 in the whole message, those of forwarded messages included |
| Header fields | 20,000 in the whole message, those of every part included |

A message that passes one of them is not read at all: over SMTP it is refused
with `554 5.6.0`, fetched from another provider it counts as refused (and is
cleared there like any other refused message), IMAP `APPEND` answers `NO`, and
JMAP `Email/import` answers `invalidEmail`; moving mail over from another
server and restoring a backup leave it out, say so in the log, and go on with
the rest. A message made of parts whose boundary never comes, over and over, is
refused the same way.

The reason is the server itself: a message nested tens of thousands of times
over used to take the whole server down while it was read, and millions of tiny
header fields or parts cost gigabytes of memory.

## Accounts: people and services

*Server → Accounts* holds both. A **person** signs in to the portal and may use
everything. A **service** is a mailbox that belongs to a program: a backup
script that sends a report, a shop that sends receipts, a monitoring job. It
never signs in to the portal, the admin panel or the webmail, never has a
password of its own (setting one is refused with `serviceAccount`), and gets in
only with app passwords, which an admin creates on its page. Not even a
password kept at an LDAP directory counts for a service. A
[shared mailbox](groups.md#shared-mailboxes) is a service with members.

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
their second factors, passkeys, open sessions, password links and apps signed in
with OAuth go, since none of them has anything left to sign in to, and so does
the tie to an LDAP directory or OpenID Connect provider. So do the folders
others shared with them and the shared mailboxes they were a member of: only
people share with people. What the person set up for their own mail stops too
— forwarding, fetched mailboxes, moves from another provider, updates of
subscribed calendars and the active mail rule — and their masked addresses are
switched off (they stay with the account); see
[groups.md](groups.md#turning-an-account-into-one). The way back is the same button, and afterwards the
account needs a new password or an invitation link. How a person or a service
becomes a shared mailbox is in [groups.md](groups.md#turning-an-account-into-one).

The same from the terminal:

```bash
docker compose exec uwumail uwumail-server account add reports@example.com --service --name "Reports"
docker compose exec uwumail uwumail-server account protocols reports@example.com \
  --imap off --jmap off --redirect me@example.com
docker compose exec uwumail uwumail-server account service someone@example.com on
docker compose exec uwumail uwumail-server account shared someone@example.com on --sender me@example.com
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
managesieve = "[::]:4190"  # ManageSieve for mail rules, STARTTLS (docs/sieve.md)
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
max_connections_per_client = 20  # at once from one address (IPv6: one /64); trusted_relays are not limited
verify_senders = true         # SPF, DKIM, DMARC for incoming mail
trusted_relays = []           # servers in front that forward mail to us, e.g. ["10.0.0.5"]
enforce_dmarc_reject = true   # otherwise p=reject failures go to Junk
reveal_client_ip = false      # keep senders' IP and device name out of headers
allow_external_forwarding = true  # people may forward to other servers (after the address confirms); forwarding addresses set up by admins always may

[spam]                        # see spam-filter.md
enabled = true
blocklists = true             # ask Spamhaus ZEN, SpamCop and Barracuda
uri_blocklists = false        # ask SURBL and URIBL about link domains; free for small servers only, no public resolvers
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

# Remote pictures in messages, fetched by the server: see "Remote pictures through a VPN" above.
[egress]
proxy = ""             # "http://gluetun:8888" or "socks5://user:password@host:1080"; empty: straight out
fallback = "block"     # block | direct: what happens while the proxy is away
pictures = true        # remote pictures, sender logos, linked contact photos, Libravatar and one-click unsubscriptions take the proxy
updates = false        # the check for new versions takes it
fetch = false          # fetching from other providers (mailboxes, calendars, contacts) takes it
image_cache_mb = 1024  # the shared cache of remote pictures on disk, in MB; 0 keeps none
assist = false         # the AI assistant's requests to providers on the internet take it (docs/llm.md)

# Reading the text in pictures with Tesseract (Email/imageText), see docs/jmap-image-text.md.
[ocr]
enabled = true         # does nothing while Tesseract is missing; the Docker image has it
command = "tesseract"  # a name looked up in PATH, or a path
languages = "deu+eng"  # Tesseract's languages; their data has to be installed

# Daily TLS reports (RFC 8460) to the domains mail went to, see docs/tls-reports.md.
[reports]
send_tls_reports = true

[tone]
language = "de"        # de | en | fr | nl | ja | zh
internal = "playful"   # playful | neutral: mail to our own people
external = "neutral"   # neutral | light: mail to everyone else

# Name, colour and mascot instead of UwUMail's own, see docs/branding.md.
# [brand]
# name = "Post & Co"
# color = "#0ea5e9"
# mascot = true

[log]
format = "text"        # text | json
level = "info"
# Sending the log to Grafana Loki: see "Sending the log to Grafana Loki" above.
# [log.loki]
# enabled = false

# Prometheus metrics under /metrics, see docs/metrics.md. Off unless switched on.
# [metrics]
# enabled = true
# token = "a-long-random-token"      # sent as "Authorization: Bearer …"
# allowed_networks = ["10.0.0.0/8"]  # when set, only from these networks

# Logging in to the portal at an OpenID Connect provider or with an LDAP directory's
# password, see docs/login-oidc-ldap.md.
# [auth.oidc]
# enabled = false
# issuer = "https://auth.example.com/application/o/uwumail/"
# client_id = "uwumail"
# [auth.ldap]
# enabled = false
# url = "ldaps://ldap.example.com"

# Fetched mailboxes signing in at Microsoft and Google, see docs/fetch.md.
# Microsoft works without anything here; Google needs a client of your own.
# [fetch.oauth]
# microsoft_client_id = ""     # empty: the client UwUMail ships with
# google_client_id = "1234-abc.apps.googleusercontent.com"
# google_client_secret = "..."
```

An SMTP session is closed after `smtp.timeout_secs` without a byte, and also when it gets nowhere:
it has three minutes to log in or finish a message, and three more after each, and a message has
ten minutes to arrive once DATA or the first BDAT chunk began. Real clients get there in seconds; a
client that sends a NOOP now and then only to keep its connection does not keep it. One client
address may have `smtp.max_connections_per_client` connections at once on all SMTP ports together;
behind something that hides the clients' addresses (a proxy that makes every connection come from
one address), raise it or list that address in `trusted_relays`. IMAP allows 50 connections at once per
client address and ManageSieve 20, counted apart from SMTP; addresses in `smtp.trusted_relays` are
not limited there either. Connections through the UwUMail Gateway count for the client's own
address. Before logging in, IMAP takes literals of at most 8 KiB (a user name or a password).
