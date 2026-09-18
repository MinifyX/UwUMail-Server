# Installing UwUMail Server

This is how to set up UwUMail Server from nothing to the first mail in your
inbox. Plan an hour, most of it waiting for DNS.

> UwUMail is a just-for-fun project that I build for myself, and I run my own
> mail on it. It is still young: set up backups, and don't expect support. Issues
> are okay, but I might answer late or not at all.

## Pick a setup

| | A: server with a public address | B: server at home with a gateway |
| --- | --- | --- |
| Where UwUMail runs | A VPS or root server | Any machine at home (a Raspberry Pi is enough) |
| What else you need | Nothing | A small VPS for the [UwUMail Gateway](gateway.md) |
| Good when | The provider allows port 25 and lets you set reverse DNS | You have no fixed IP, port 25 is blocked, or your mail should stay at home |

In B the server dials out to the gateway; nothing has to be opened on your
router. Other mail servers and DNS only ever see the gateway's address.

## What you need

- **A domain** where you can edit DNS records. The server gets a name in it,
  for example `mail.example.com`.
- **A Linux machine for the server** (amd64 or arm64, 1 GB RAM or more) with
  Docker. Ubuntu 24.04 or newer is what I use. Another web server on its ports
  80 and 443 is fine, see step 4.
- **A: the server** needs a fixed public IPv4 address (IPv6 as well is better),
  port 25 open in both directions, and reverse DNS you can set.
- **B: the gateway VPS** needs the same: a fixed public address, reverse DNS
  you can set and outgoing port 25. The smallest plan of most providers is
  plenty; the ready-made gateway is built for amd64. Avoid Hetzner Cloud for
  this, see [gateway.md](gateway.md#what-you-need).

## 1. Install Docker on the server

On Ubuntu 24.04 or newer:

```bash
sudo apt update
sudo apt install -y docker.io docker-compose-v2
docker compose version
```

On other systems, follow [Docker's guide](https://docs.docker.com/engine/install/).
`docker compose version` has to answer with version 2 or newer.

## 2. B only: install the gateway on the VPS

The gateway comes first, because the server pairs with it when it starts. On
the VPS (Debian 12 or newer, Ubuntu 24.04 or newer, amd64), and that VPS should
be the gateway's alone: no second mail or web server, no Plesk. It wants ports
25, 80, 443, 465, 587 and 993 for itself, and everything else there shares its
fate.

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz.sha256
sha256sum -c uwumail-gateway-linux-amd64.tar.gz.sha256
tar -xzf uwumail-gateway-linux-amd64.tar.gz
sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway
```

It installs the gateway as a systemd service and prints a **pairing code**
(`uwugw1…`). Keep it for step 4; `sudo uwumail-gateway code` shows it again.

Open TCP 25, 80, 443, 465, 587 and 993 and UDP 443 on the VPS. The
[gateway guide](gateway.md#install-the-gateway) has a firewall example and
explains what the gateway does and doesn't see.

## 3. DNS and reverse DNS

Create these two records now, before the server starts, so the certificate
works at the first try:

| Record | A: points to | B: points to |
| --- | --- | --- |
| `mail.example.com A` | the server's IPv4 address | the gateway's IPv4 address |
| `mail.example.com AAAA` (if you have IPv6) | the server's IPv6 address | the gateway's IPv6 address |

Set the **reverse DNS** (PTR) of the same addresses to `mail.example.com` in
your provider's panel. Without it, big mail providers refuse your mail.

The MX, SPF, DKIM and DMARC records come in step 5; the assistant shows them.

## 4. Start UwUMail

```bash
sudo mkdir -p /opt/uwumail && cd /opt/uwumail
sudo curl -fsSLO https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/compose.yaml
sudo curl -fsSL -o .env https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/.env.example
sudo nano .env
```

In `.env`, set at least the host name. The e-mail address is optional:
Let's Encrypt warns you there before a certificate expires.

```bash
UWUMAIL_HOSTNAME=mail.example.com
UWUMAIL_ACME_EMAIL=you@example.org
UWUMAIL_LANGUAGE=en
UWUMAIL_VERSION=latest
```

**B:** also set the pairing code from step 2. The server then pairs with the
gateway when it starts, before there is an admin account.

```bash
UWUMAIL_GATEWAY_CODE=uwugw1…
```

The stock `compose.yaml` hands that line to the server as
`UWUMAIL_GATEWAY__CODE`, with two underscores. With a compose file of your own,
set that variable under `environment:`; Compose does not pass `.env` into the
container by itself.

If another web server (Caddy, Traefik, nginx) already uses ports 80 and 443 on
this machine, move UwUMail's web ports in `.env`, as a port or as address:port:

```bash
UWUMAIL_HTTP_BIND=127.0.0.1:8081
UWUMAIL_HTTPS_BIND=8443
```

In B that is all: the web arrives through the tunnel, and the other web server
has nothing to do with UwUMail. In A it has to pass `mail.example.com` on to
UwUMail as a reverse proxy, see
[deployment.md](deployment.md#behind-a-reverse-proxy). Never point a reverse
proxy at UwUMail's port 80: it only redirects to HTTPS, and the browser ends up
in a loop.

**A:** open these ports in the provider's firewall: TCP 25, 80, 443, 465, 587
and 993.

**B:** nothing to open at home, and the web ports don't have to be reachable
from outside at all: the web arrives through the tunnel, like the mail. The
server only needs to reach the gateway over UDP port 443.

Then start it:

```bash
sudo docker compose up -d
sudo docker compose logs uwumail | grep "one-time code"
```

The last line shows a one-time code for the setup assistant. The server makes a
new one on every start until the first admin exists, so always use the newest.

## 5. Setup assistant

**A:** open `https://mail.example.com/setup`.

**B:** wait until the log shows the tunnel and the certificate:

```bash
sudo docker compose logs uwumail | grep -E "connected to the UwUMail Gateway|got a fresh certificate"
```

Then open `https://mail.example.com/setup`; it works through the gateway.

If the name doesn't work yet, `https://<address of the server>/setup` works in
both setups, with the port when you moved 443, for example
`https://192.168.1.10:8443/setup`. The browser warns about the certificate
there. Use that address only to get set up: passkeys and the Apple profile
only work with `https://mail.example.com`, and invitation links you copy in
the portal carry the address you opened it with.

Enter the one-time code, then the assistant

1. creates your admin account and your first domain,
2. checks how the server is connected (*Reachability*) and recommends a gateway
   or sending directly. **B:** the gateway shows as paired already. If you
   didn't put the code into `.env`, choose *Through a gateway* and paste it
   here instead,
3. shows the DNS records for the domain and checks them; for a domain at
   Cloudflare it can add them with an API token that is not stored,
4. checks whether mail gets out and whether port 25 answers,
5. checks reverse DNS and, only when you ask, the blocklists,
6. sends a test mail, optionally also to another address of yours. Reply to it
   from there: when the reply arrives, mail from outside works.

The certificate comes by itself; the log says `got a fresh certificate` when it
is there. In A the server asks Let's Encrypt when it starts. If
`mail.example.com` didn't reach it then, on port 80 or through a reverse proxy
you set up afterwards, it tries again within the hour, and
`sudo docker compose restart uwumail` makes it try right away. In B it asks as
soon as the tunnel to the gateway is connected, also when you pair in the
assistant; no restart needed.

Everything the assistant checked stays in the portal under *Server → Setup*.
More domains and people are added under *Domains* and *People*.

## 6. Mail apps

- **UwUMail apps** only need `https://mail.example.com`.
- **Thunderbird, Outlook and most others** find the settings themselves from
  the address, once `autoconfig.example.com` and `autodiscover.example.com`
  point to the same address as `mail.example.com`.
- **iPhone, iPad and Mac:** *My account → Connect mail apps* makes a profile with
  mail, calendars and contacts.
- **Everything else:** IMAP on port 993 (TLS), sending on 465 (TLS) or 587
  (STARTTLS), all at `mail.example.com`, logging in with the full address.

Details, calendars and contacts: [deployment.md](deployment.md#mail-apps).

## 7. Backups

Set them up before you rely on the server: *Server → Backups* backs up every
night to an SFTP server such as a NAS, deduplicated and encrypted. Keep the
recovery key somewhere else than the server. See [backups.md](backups.md).

## Updates

*Server → Updates* in the portal shows when a new version is out, with what
changed. To update, in `/opt/uwumail`:

```bash
sudo docker compose pull && sudo docker compose up -d
```

Mail stays, and database changes run by themselves. For the gateway, the portal
shows the matching command.

With the helper below installed, that page updates the server itself instead:
a button, or a day and time. It makes a backup first — if that fails, nothing
is touched — and it keeps half an hour clear on either side of the nightly
backup. Should the new version not come up, the old tag goes back. Without the
helper nothing updates on its own.

That does not fetch a new `compose.yaml`. Options in `.env` that came later,
like `UWUMAIL_GATEWAY_CODE` or `UWUMAIL_HTTP_BIND`, need the current file; see
*A line in `.env` changes nothing* below.

### Buttons instead of commands (optional)

The server's container cannot touch the machine it runs on. It is distroless,
read-only, unprivileged, and every capability is dropped but the one it needs
for the low mail ports — which is most of what makes a break-in worth little,
so it stays that way.

A small helper beside it can, and then the portal shows what the system has
waiting and installs it with a button:

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-host.tar.gz
tar -xzf uwumail-host.tar.gz
sudo bash uwumail-host/install.sh --dir /opt/uwumail
cd /opt/uwumail && sudo docker compose up -d
```

What the container may ask the helper for is one of three things — install the
system's updates, restart the machine, pull a new UwUMail — plus a version
number that has to look like one. Never a command, never a path, never an
address. The docker socket stays where it is: handing that to a container is
handing it the machine.

`sudo bash install.sh --check` says how things stand, `--remove` takes it back
out. If anything else runs on this machine, the portal says so above the button
and installs anyway when you insist: `apt` restarts those too, and a reboot
takes them with it.

## When something doesn't work

- `sudo docker compose logs --tail 100 uwumail` shows what the server is doing.
  On the gateway VPS: `sudo journalctl -u uwumail-gateway`.
- The admin overview in the portal shows the health of DNS, certificate,
  sending, storage, logins and the gateway; *Server → Setup* runs the checks
  again.
- **Port 80 or 443 already in use:** `docker compose up` fails with
  `address already in use` or `port is already allocated` when another web
  server has them. Put the two lines `UWUMAIL_HTTP_BIND` and
  `UWUMAIL_HTTPS_BIND` from step 4 into `.env` and run
  `sudo docker compose up -d` again.
- **A line in `.env` changes nothing:** `UWUMAIL_GATEWAY_CODE`,
  `UWUMAIL_HTTP_BIND` and `UWUMAIL_HTTPS_BIND` only work with a `compose.yaml`
  that mentions them; one from before they existed ignores them without a
  word. `grep -c UWUMAIL_HTTP_BIND compose.yaml` has to answer 1 or more. If it
  doesn't, fetch the current file with the `curl` line for `compose.yaml` from
  step 4 (copy the old one first if you changed it) and run
  `sudo docker compose up -d`. `.env` and your mail stay.
- **The browser says too many redirects:** a reverse proxy points at UwUMail's
  port 80, which only redirects to HTTPS. Point it at the proxy listener, see
  [deployment.md](deployment.md#behind-a-reverse-proxy). In B, remove the entry
  from the proxy instead; it is not needed.
- **B: `https://mail.example.com` doesn't open:** the gateway passes the web on
  only while the server is connected, so the log has to show
  `connected to the UwUMail Gateway`. If it says nothing about the gateway, the
  code didn't reach the server: it is missing in `.env`, or `compose.yaml` is
  an older one (see above). If it says `could not reach the UwUMail Gateway`,
  check that UDP port 443 is open on the VPS. After a change in `.env`,
  `sudo docker compose up -d` applies it; a restart does not.
- **B: the log says `the UwUMail Gateway refused this server`:** the gateway is
  paired with another key. *Forget gateway* in the portal and a new data volume
  both make a new key. Run `sudo uwumail-gateway unpair` on the VPS and use the
  new code. A code in `.env` wins over a pairing made in the portal at the next
  start, so don't leave an old one there.
- **No certificate:** `mail.example.com` must point to the server (A) or the
  gateway (B). In A, port 80 must be reachable from outside; in B, the tunnel
  must be connected. The server tries again within the hour.
- **Mail doesn't go out:** port 25 blocked by the provider, or reverse DNS
  missing. *Server → Queue* shows why a message is still waiting.
- **Mail doesn't come in:** check the MX record, and in A that port 25 is open.

## More

- [deployment.md](deployment.md): every DNS record, MTA-STS, reverse proxy,
  running next to an existing mail server
- [configuration.md](configuration.md): all settings
- [gateway.md](gateway.md): how the gateway works
- [spam-filter.md](spam-filter.md): how the spam filter decides
- [migrating-from-mailcow.md](migrating-from-mailcow.md): moving over from mailcow
