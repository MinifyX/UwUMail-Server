# Deployment

> UwUMail Server is in early development. Run it for testing, not yet for the
> only copy of your mail.

## What you need

- A machine with a public IPv4 (and ideally IPv6) address, 1 GB RAM or more
  (4 GB if you want the optional virus scanner later), amd64 or arm64.
- **Port 25 open in both directions.** Some providers block it until you ask.
- **Reverse DNS** (PTR) of the IP pointing to your server's host name.
- A domain where you can edit DNS records.
- Docker with Compose.

At home without a fixed IP or with a blocked port 25? Put a
[UwUMail Gateway](gateway.md) on a small VPS in front of your server. Or only
send through a relay (the setup assistant offers it, or `[delivery.relay]`).

## Start

```bash
mkdir uwumail && cd uwumail
curl -O https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/compose.yaml
curl -o .env https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/.env.example
# edit .env: UWUMAIL_HOSTNAME=mail.example.com
docker compose up -d
docker compose logs uwumail | grep setup
```

The server gets a Let's Encrypt certificate as soon as `mail.example.com`
points to it and port 80 is reachable. Until then it uses a self-signed one.

## Setup assistant

While there is no admin, the server writes a one-time code to its log on every
start. Open `https://mail.example.com/setup` and enter it. The assistant

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
under *Server → Setup*.

The port 25 check calls the server on its own public address. It cannot see a
provider blocking port 25 inbound, and some routers cannot reach themselves
that way; the reply to the test mail is the reliable answer.

## Domains and accounts on the command line

Instead of the assistant, or for more domains and people:

```bash
docker compose exec uwumail uwumail-server domain add example.com
docker compose exec uwumail uwumail-server account add you@example.com --name "You" --admin
```

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
portal checks all of them and, for domains at Cloudflare, can add them.

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
  makes an app password for the device and a configuration profile with it. The
  download link works once and for ten minutes. On an iPhone, install the
  profile in *Settings* under *Profile Downloaded*. The profile is not signed,
  so iOS shows it as unverified.

## Behind a reverse proxy

Mail ports are always handled by UwUMail itself. For the web part:

```toml
[listen]
http = ""
https = ""
proxy = "[::]:8080"

[tls]
mode = "files"
cert_file = "/certs/mail.example.com.crt"
key_file = "/certs/mail.example.com.key"
```

Point the proxy at port 8080 and mount the certificate your proxy manages
(Traefik, Caddy and Nginx Proxy Manager can all export it) so STARTTLS and
port 465 use the same certificate. It is reloaded when the files change.

Alternatively keep `mode = "acme"` with only the proxy listener: Let's Encrypt
follows the proxy's redirect to HTTPS, and the proxy forwards
`/.well-known/acme-challenge/` to UwUMail like every other path.

For MTA-STS, also route `mta-sts.<domain>` of each domain to port 8080; UwUMail
picks the domain from the host name.

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
   real client addresses.
5. **DNS for the test domain:** MX to the existing mail server's host name,
   SPF with the relay's IP address, the two DKIM keys from `domain add`, and a
   DMARC record (start with `p=none`). A subdomain needs its own DMARC record
   when the parent domain says `sp=reject`, or its mail is rejected.

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

```bash
docker compose pull && docker compose up -d
```

Database migrations run automatically on start.
