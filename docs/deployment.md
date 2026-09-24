# Deployment

> New here? [install.md](install.md) walks through the whole setup step by step.
> This page is the reference behind it.
>
> UwUMail Server is young. I run my own mail on it, but set up
> [backups](backups.md) before you rely on it.

## What you need

- A machine with a public IPv4 (and ideally IPv6) address, 1 GB RAM or more
  (3 GB or more with the optional [virus scanner](antivirus.md)), amd64 or arm64.
- **Port 25 open in both directions.** Some providers block it until you ask.
- **Reverse DNS** (PTR) of the IP pointing to your server's host name.
- A domain where you can edit DNS records.
- Docker with Compose.

At home without a fixed IP or with a blocked port 25? Put a
[UwUMail Gateway](gateway.md) on a small VPS in front of your server. Or only
send through a relay (the setup assistant offers it, or `[delivery.relay]`).

## Start

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo bash install.sh --hostname mail.example.com --email you@example.org --yes
```

That writes `/opt/uwumail` with `compose.yaml` and an `.env`, starts the server
and prints the one-time code. `--help` lists every flag, `--dir` puts it
somewhere else. By hand it is the same three files:

```bash
mkdir uwumail && cd uwumail
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/compose.yaml
curl -fsSL -o .env https://github.com/MinifyX/UwUMail-Server/releases/latest/download/env.example
# edit .env: UWUMAIL_HOSTNAME=mail.example.com
docker compose up -d
docker compose logs uwumail | grep "one-time code"
```

The server gets a Let's Encrypt certificate as soon as `mail.example.com`
points to it and port 80 is reachable. Until then it uses a self-signed one.
Names that mail apps may already know from an earlier server, like
`imap.<domain>`, `smtp.<domain>`, `mail.<domain>`, `autoconfig.<domain>` and
`autodiscover.<domain>`, join the certificate within 15 minutes once they point
here too.

## Setup assistant

While there is no admin, the server writes a one-time code to its log on every
start. Open `https://mail.example.com/setup` and enter it.

Behind a [gateway](gateway.md#pair-your-server) the name only answers once the
server is paired, so the pairing code goes into `.env` before the first start.
`https://<address of the server>/setup` works as well, with the port when
`UWUMAIL_HTTPS_BIND` moved 443; the browser warns about the certificate.

The assistant

1. creates the first domain (with DKIM keys) and your admin account,
2. shows the DNS records and checks them; if the domain is at Cloudflare, it can
   add them with an API token (permission *Zone → DNS → Edit*) that is used for
   that one request and never stored,
3. checks whether mail gets out (port 25 or the relay, which can be set up right
   there) and whether port 25 answers on the public addresses,
4. checks reverse DNS and, only when asked, the Spamhaus, SpamCop and Barracuda
   blocklists,
5. sends a test mail to your new mailbox and, optionally, to another address of
   yours; your reply from there shows that mail from outside arrives.

The code stops working once the admin exists. The checks stay in the portal
under *Server → Overview → Mail flow*.

The port 25 check calls the server on its own public address. It cannot see a
provider blocking port 25 inbound, and some routers cannot reach themselves
that way; the reply to the test mail is the reliable answer.

## Domains and accounts on the command line

Instead of the assistant, or for more domains and people:

```bash
docker compose exec uwumail uwumail-server domain add example.com
docker compose exec uwumail uwumail-server account add you@example.com --name "You" --admin
docker compose exec uwumail uwumail-server account admin someone@example.com on
```

A mailbox for a program rather than a person gets `--service`: no portal login,
app passwords only, and the protocols it may use are switches of their own. See
[configuration.md](configuration.md#accounts-people-and-services).

`domain add` prints the DNS records to create:

| Record | Purpose |
| --- | --- |
| `example.com MX 10 mail.example.com.` | Where mail for the domain goes |
| `example.com TXT "v=spf1 mx -all"` | Only this server may send for the domain |
| `uwu…r._domainkey` and `uwu…e._domainkey` TXT | DKIM keys (RSA and Ed25519) |
| `_dmarc.example.com TXT "v=DMARC1; p=quarantine; …; rua=mailto:dmarc-reports@example.com"` | What receivers do with forged mail, and where they send reports |
| `mail.example.com A/AAAA` and PTR | The server itself |

Recommended, mail works without them:

| Record | Purpose |
| --- | --- |
| `_smtp._tls.example.com TXT "v=TLSRPTv1; rua=mailto:tls-reports@example.com"` | Reports about TLS connections to the server |
| `_jmap._tcp.example.com SRV 0 1 443 mail.example.com.` | Apps like UwUMail find the server from the address alone |
| `_imaps._tcp` (993) SRV | Mail apps find where to read mail |
| `_submissions._tcp` (465) and `_submission._tcp` (587) SRV | Mail apps find where to send |
| `autoconfig.example.com` and `autodiscover.example.com` A/AAAA (or CNAME to `mail.example.com`) | Thunderbird and Outlook set themselves up; the certificate must cover these names too |

Show them again any time with `uwumail-server domain dns example.com`. The
portal checks all of them and, for domains at Cloudflare, can add them. TXT
values go there in quotes, split into several strings when they outgrow the 255
bytes one string may hold, the way Cloudflare's own dashboard writes them; a
record that is already right but sits there without quotes gets them.

A record that works but does not read the way UwUMail would write it — a DMARC
policy with other tags, reports going to another address — counts as fine, and
the check leaves it at that. The Cloudflare button offers it under *Bring the
spelling in line with UwUMail*, unticked; only a tick rewrites it. For MX and
SPF that means exactly our value: another sender listed in SPF, or a second MX,
would fall away.

## MTA-STS and reports

MTA-STS tells other mail servers to deliver to your domain only over TLS with
a valid certificate. Switch it on per domain in the portal (*Domains → the
domain → MTA-STS*). It starts in testing mode, where senders only report
problems; after 14 days without failures the portal suggests enforce.

With MTA-STS on, the domain needs three more records, which the DNS check
lists:

- `_mta-sts.example.com TXT "v=STSv1; id=…"`: the id changes with the policy.
- `mta-sts.example.com CNAME mail.example.com.`: senders fetch the policy from
  `https://mta-sts.example.com/.well-known/mta-sts.txt`, which UwUMail serves.
  With Let's Encrypt, the server adds this name to its certificate once it
  points here. Behind a reverse proxy, route the name to UwUMail like the
  hostname and let the proxy handle its certificate.
- `_smtp._tls.example.com`, as above.

If another mail server receives first (`smtp.trusted_relays`), the policy lists
its MX hosts too, and their certificates are what senders check.

The server reads DMARC aggregate reports sent to `dmarc-reports@` and TLS
reports sent to `tls-reports@` each domain itself, unless you gave someone
that address. The domain page shows how much mail in the domain's name passed
DMARC, from which addresses, and which TLS connections failed. Only the numbers
are kept, for 180 days.

When delivering, UwUMail follows other domains' MTA-STS policies: with an
enforced policy it only delivers to the MX hosts listed and only with a valid
certificate, and otherwise tries again later.

## Mail apps

| | Server | Port | Security |
| --- | --- | --- | --- |
| Reading (IMAP) | `mail.example.com` | 993 | TLS |
| Sending | `mail.example.com` | 465 | TLS |
| Sending | `mail.example.com` | 587 | STARTTLS |

Log in with the full address and password. With a second factor (authenticator
app or passkey), or when switched on under *Security* in the portal, mail apps
need an app password instead; the main password then only works in the portal.
JMAP apps (like UwUMail) only need
`https://mail.example.com`; they find everything else at `/.well-known/jmap`.
IMAP is only offered with TLS on port 993, not with STARTTLS on 143.

Most apps find these settings themselves:

- **Thunderbird** and others ask for `/.well-known/autoconfig/mail/config-v1.1.xml`
  on the server or `/mail/config-v1.1.xml` on `autoconfig.example.com`.
- **Outlook** posts to `/autodiscover/autodiscover.xml` on
  `autodiscover.example.com`. The answer only names the servers; it does not
  say whether a mailbox exists.
- **iPhone, iPad and Mac:** *My account → Connect mail apps → Download profile*
  makes an app password for the device and a configuration profile with mail,
  calendars and contacts. The download link works once and for ten minutes. On
  an iPhone, install the profile in *Settings* under *Profile Downloaded*. The
  profile is not signed, so iOS shows it as unverified.

## Calendars and contacts

CalDAV and CardDAV run on the HTTPS port next to the portal and JMAP:

| | Address |
| --- | --- |
| Server (most apps find the rest) | `https://mail.example.com/` |
| Principal | `https://mail.example.com/dav/principals/you@example.com/` |
| Calendars | `https://mail.example.com/dav/calendars/you@example.com/` |
| Address books | `https://mail.example.com/dav/addressbooks/you@example.com/` |

Log in with the full address and an app password that may use *Calendars and
contacts* (the Apple profile makes one), or the account password while mail
apps may still use it. Everyone starts with one calendar and one address book;
apps can add more. Sync tokens let apps like DAVx5 and the iPhone fetch only
what changed. Scheduling (invitations sent by the server) and shared calendars
are not there yet.

## Behind a reverse proxy

Mail ports are always handled by UwUMail itself. A reverse proxy only carries
the web part, and its upstream is the **proxy listener** (`listen.proxy`, for
example port 8080). That one serves the whole site over plain HTTP, without a
redirect and without HSTS.

Never point a proxy at UwUMail's port 80. That port answers certificate
challenges and redirects everything else to `https://mail.example.com/`, which
behind a proxy goes round in circles. When a request there was already HTTPS at
the proxy, UwUMail answers with a short page (HTTP 421) that says so instead.

The proxy has to pass the `Host` header on unchanged and set `X-Forwarded-For`
and `X-Forwarded-Proto`. Caddy's `reverse_proxy` does both by itself.

**With the stock `compose.yaml`:** the files for a proxy that runs in Docker on
the same machine are in [`deploy/behind-proxy`](../deploy/behind-proxy). Copy
`compose.proxy.yaml` next to `compose.yaml` and add to `.env`:

```bash
COMPOSE_FILE=compose.yaml:compose.proxy.yaml
UWUMAIL_HTTP_BIND=127.0.0.1:8081
UWUMAIL_HTTPS_BIND=8443
```

The first line loads the override. It switches the proxy listener on, trusts
the proxy's address and creates the Docker network `uwumail-proxy`, which the
proxy joins with the fixed address `172.30.25.2` to reach `uwumail:8080`. Port
8080 is not published. The other two lines move UwUMail's own web ports away
from 80 and 443, as a port or as `address:port`. The header of
`compose.proxy.yaml` has the steps, and says what to change when that address
range is taken. Its end has the variant for a proxy that runs on the host
instead of in Docker, and the `Caddyfile` next to it is Caddy's side.

The mail ports move the same way when something on the machine already holds
one of them — `UWUMAIL_SMTP_BIND`, `UWUMAIL_SUBMISSIONS_BIND`,
`UWUMAIL_SUBMISSION_BIND` and `UWUMAIL_IMAPS_BIND`, all six in a table in
[configuration.md](configuration.md). They are never proxied,
though: only where UwUMail listens moves, and whatever is in front has to send
25, 465, 587 and 993 to the new ports. `install.sh` asks about every port it
finds taken, so a fresh install usually has these lines already.

**With a config file** the same looks like this:

```toml
[listen]
http = ""
https = ""
proxy = "[::]:8080"

[http]
trusted_proxies = ["172.30.25.2"]

[tls]
mode = "files"
cert_file = "/certs/mail.example.com.crt"
key_file = "/certs/mail.example.com.key"
```

`http.trusted_proxies` lists the addresses whose `X-Forwarded-For` and
`X-Forwarded-Proto` UwUMail believes. While the proxy's address is missing,
every visitor counts as the proxy: all logins share one throttle (10 failures
in 15 minutes per address), the logs and the change log show the proxy's
address, and the session cookie is set without `Secure`. To find the address,
leave the list empty and open the site through the proxy once. The server then
names it in a warning, once per start:

```bash
docker compose logs uwumail | grep trusted_proxies
```

If that is the gateway address of a Docker network (like `172.18.0.1`, when the
proxy runs on the host and reaches a published port), other traffic to
published ports can arrive from it as well: IPv6 visitors on a Docker network
without IPv6, and everything under Docker Desktop or rootless Docker. Trust it
only when every published web port is bound to loopback:
`UWUMAIL_HTTP_BIND=127.0.0.1:8081`, `UWUMAIL_HTTPS_BIND=127.0.0.1:8443` and
`127.0.0.1:8080:8080` for the proxy listener.

As an environment variable the list needs its square brackets:
`UWUMAIL_HTTP__TRUSTED_PROXIES=[172.30.25.2]`. With a bare or an empty value
the server refuses to start. In a compose file, quote it (`"[172.30.25.2]"`),
or YAML reads it as a list of its own.

The mail ports need a certificate for the host name too. Both ways need the
public name to point to the proxy:

- `mode = "files"`, as above: mount the certificate your proxy manages
  (Traefik, Caddy and Nginx Proxy Manager can all export it), so the mail ports
  use the same one. It is reloaded when the files change.
- `mode = "acme"`, which is the default and what the stock `compose.yaml` runs
  with: Let's Encrypt follows the proxy's redirect to HTTPS, and the proxy
  forwards `/.well-known/acme-challenge/` to UwUMail like every other path.
  Every name on the certificate is checked that way, so `imap.<domain>` and
  the like only join it when the proxy routes them to UwUMail as well. UwUMail
  asks a few seconds after it starts. When the proxy only learned the name
  after that, the first try has failed: `docker compose restart uwumail` makes
  it ask again once the site opens through the proxy, otherwise it retries
  within the hour.

For MTA-STS, also route `mta-sts.<domain>` of each domain to port 8080; UwUMail
picks the domain from the host name. The same goes for `autoconfig.<domain>`
and `autodiscover.<domain>`, where mail apps look up their settings.

### With a gateway (setup B)

A server at home behind a [UwUMail Gateway](gateway.md) needs none of this,
also when Caddy or another web server already holds ports 80 and 443 on the
machine. The public name points to the gateway, and the gateway's ports 80 and
443 end in UwUMail through the tunnel, so the other web server is not in the
public path. Keep `tls.mode = "acme"`, because the challenge arrives through
the tunnel as well, and only move UwUMail's web ports out of the way in `.env`:

```bash
UWUMAIL_HTTP_BIND=127.0.0.1:8081
UWUMAIL_HTTPS_BIND=8443
```

`https://<address in your network>:8443` then reaches the portal without the
gateway; the browser warns about the certificate. That is a fallback for setup
and admin work, not an address to hand out: passkeys are bound to
`https://mail.example.com` without a port, the Apple profile points calendars
and contacts at port 443 of the host name, and invitation links you copy in the
portal carry the address you opened it with.

An entry for the name in the local proxy is only of use with a local DNS entry
that points the name at the home machine. The proxy cannot get a public
certificate for it, because Let's Encrypt's checks end at the gateway. That
leaves a certificate from the proxy's own CA (Caddy: `tls internal`) or a DNS
challenge, which in Caddy needs a build with a DNS plugin. With its own CA,
every device has to trust the root certificate: through the gateway UwUMail
sends HSTS for a year, and a browser that has seen it does not let you click
through a certificate warning for the name. The simplest is no local DNS
entry: use the gateway from home too.

## Next to an existing mail server

You can try UwUMail on a (sub)domain while an existing mail server such as
Mailcow keeps port 25 and all other domains. Ready-made files:
[`deploy/next-to-mailserver`](../deploy/next-to-mailserver).

1. **Existing mail server:** add the test domain as a relay domain (Mailcow:
   *Domains → Add domain → Relay this domain, relay all recipients*) and a
   transport map `uwu.example.com → [192.0.2.30]:25` (Mailcow: *Routing →
   Transport maps*). Without *relay all recipients* Mailcow answers
   `User unknown in relay recipient table` and nothing reaches UwUMail.
2. **UwUMail:** put the existing mail server's address into
   `smtp.trusted_relays`. SPF and DMARC are then checked against the server
   that delivered to it, read from its Received header.
3. **Outgoing mail:** `[delivery.relay]` with the relay you already use; the
   password goes into `.env` as `RELAY_PASSWORD`.
4. **Web:** the reverse proxy forwards the UwUMail host name to port 8080;
   put its address into `http.trusted_proxies` so login throttling sees the
   real client addresses. Port 8080 is plain HTTP, so give it to the proxy
   only: `UWUMAIL_PROXY_BIND` in `.env` binds it to one address, see the
   comment in `compose.yaml`.
5. **DNS for the test domain:** MX to the existing mail server's host name,
   SPF with the relay's IP address, the two DKIM keys from `domain add`, and a
   DMARC record (start with `p=none`). A subdomain needs its own DMARC record
   when the parent domain says `sp=reject`, or its mail is rejected.

The [virus scanner](antivirus.md) is in these files too, behind the same
profile as everywhere else: `docker compose --profile antivirus up -d`.

Mail apps in your own network connect straight to the UwUMail machine on 993,
465 or 587; a local DNS entry for the host name keeps certificates valid.

## Checking a live server

`scripts/live-check.mjs` uses a server the way the app does: JMAP session over
HTTPS, mailboxes, push and sending. Create a test account for it and keep its
password in a file:

```bash
UWUMAIL_URL=https://mail.example.com UWUMAIL_LOGIN=test@example.com \
UWUMAIL_PASSWORD_FILE=test.password node scripts/live-check.mjs \
  --smtp 192.0.2.30:587 \
  --to check-auth@verifier.port25.com --wait-reply-from port25.com
```

`--smtp` also logs in on the submission port and checks the certificate.
Port25's verifier answers with SPF, DKIM and DMARC results as seen from the
outside; that reply also proves that incoming mail reaches the server.

## Updates

`UWUMAIL_VERSION` in `.env` picks what the server follows:

| Tag | |
| --- | --- |
| `latest` | stable releases |
| `beta` | every release, stable or beta |
| `edge` | every commit on `main` that passed CI |
| `0.1.0` | exactly this version |

With the machine's helper (the installer sets it up; see
[install.md](install.md)) *Server → Overview → Updates* has a button for it: after the
password, the helper fetches `update.sh` from the newest release, checks its
`sha256` and runs it here, and the page follows its output while the server is
replaced. From the button it asks nothing: it does not offer the virus scanner
(that is under *Spam filter*) and goes on without a backup when no backup server
is set up. Without the helper, or with one from before 0.9.3, it is the command:

```bash
cd /opt/uwumail && sudo bash update.sh
```

A server set up before 0.4.0 fetches the script once first:
`sudo curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/update.sh`.

Database migrations run automatically on start. Once a day the server asks
GitHub what is newer on its channel (for `edge`: which commits came since) and
shows it under *Server → Overview → Updates* with the changes. The check can be switched
off there. Nothing installs itself: the update happens when somebody presses
the button or runs the script.

What `update.sh` does, in order:

1. Fetches `update.sh` from the newest release, checks its `sha256`, and if it
   differs from itself, replaces itself and hands over to the new one.
2. Fetches `compose.yaml` the same way. When the one here is the one it put
   there, it is replaced. When it was edited, what it understands moves into
   `.env` — a changed web port, a pinned image tag, a virus scanner without its
   profile — and anything else stops the update with a diff. `--force` takes the
   new file and keeps yours as `compose.yaml.bak`.
3. Runs `backup run` in the container, unless `--no-backup`. Without a backup
   server set up it says so and asks whether to go on.
4. Offers the virus scanner when it is not there yet and the machine has the
   memory for it (`--no-antivirus` to skip the question).
5. `docker compose pull` and `up -d`, then waits up to two minutes for the
   server's own health check.
6. If it does not answer, `UWUMAIL_VERSION` goes back to the version that ran
   before and the container is recreated from it, and the script ends with exit
   code 3 (the portal shows that as *rolled back*).
7. Where the machine's helper is installed, brings it to the newest release as
   well.

Two things worth knowing about that way back:

- It pins the exact version in `.env`. A server that followed `latest` follows
  `0.3.0` afterwards; take the line out again once the trouble is understood.
- It puts the image back, nothing else. Migrations only ever run forwards, so an
  older binary may find a newer database. The backup from just before is the
  real way back.

Releases come from tags: I add a section for the version to `CHANGELOG.md`,
push the tag `v0.1.0` (or `v0.2.0-beta.1`), and CI builds the image with its
tags and publishes a GitHub release with the notes, the two scripts, the stock
`compose.yaml` and `env.example` (each with a `.sha256`), the host helper and
the gateway for amd64.
