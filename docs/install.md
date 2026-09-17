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
  Docker. Ubuntu 24.04 or newer is what I use.
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

## 2. Start UwUMail

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

Then start it:

```bash
sudo docker compose up -d
sudo docker compose logs uwumail | grep "one-time code"
```

The last line shows a one-time code for the setup assistant. The server makes a
new one on every start until the first admin exists, so always use the newest.

**A:** open these ports in the provider's firewall: TCP 25, 80, 443, 465, 587
and 993.

**B:** nothing to open at home. The server only needs to reach the gateway over
UDP port 443.

## 3. B only: install the gateway on the VPS

On the VPS (Debian 12 or newer, Ubuntu 24.04 or newer, amd64):

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz.sha256
sha256sum -c uwumail-gateway-linux-amd64.tar.gz.sha256
tar -xzf uwumail-gateway-linux-amd64.tar.gz
sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway
```

It installs the gateway as a systemd service and prints a **pairing code**
(`uwugw1…`). Keep it for step 5; `sudo uwumail-gateway code` shows it again.

Open TCP 25, 80, 443, 465, 587 and 993 and UDP 443 on the VPS. The
[gateway guide](gateway.md#install-the-gateway) has a firewall example and
explains what the gateway does and doesn't see.

## 4. DNS and reverse DNS

Create these two records now, so the certificate can be issued once the server
is reachable:

| Record | A: points to | B: points to |
| --- | --- | --- |
| `mail.example.com A` | the server's IPv4 address | the gateway's IPv4 address |
| `mail.example.com AAAA` (if you have IPv6) | the server's IPv6 address | the gateway's IPv6 address |

Set the **reverse DNS** (PTR) of the same addresses to `mail.example.com` in
your provider's panel. Without it, big mail providers refuse your mail.

The MX, SPF, DKIM and DMARC records come in the next step; the assistant shows
them.

## 5. Setup assistant

Open `https://mail.example.com/setup`. If the name doesn't point to the server
yet, `https://<address of the server>/setup` works too; the browser warns about
the certificate until the real one is there.

Enter the one-time code, then the assistant

1. creates your admin account and your first domain,
2. checks how the server is connected (*Reachability*) and recommends a gateway
   or sending directly. **B:** choose *Through a gateway* and paste the pairing
   code,
3. shows the DNS records for the domain and checks them; for a domain at
   Cloudflare it can add them with an API token that is not stored,
4. checks whether mail gets out and whether port 25 answers,
5. checks reverse DNS and, only when you ask, the blocklists,
6. sends a test mail, optionally also to another address of yours. Reply to it
   from there: when the reply arrives, mail from outside works.

The certificate follows within the hour once `mail.example.com` reaches the
server on port 80 (in B, after pairing). `sudo docker compose restart uwumail`
makes it try right away.

Everything the assistant checked stays in the portal under *Server → Setup*.
More domains and people are added under *Domains* and *People*.

## 6. Mail apps

- **UwUMail apps** only need `https://mail.example.com`.
- **Thunderbird, Outlook and most others** find the settings themselves from
  the address, once the `autoconfig` and `autodiscover` records from the
  DNS step exist.
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

The admin overview shows when a new version is out, with what changed. To
update, in `/opt/uwumail`:

```bash
sudo docker compose pull && sudo docker compose up -d
```

Mail stays, and database changes run by themselves. For the gateway, the portal
shows the matching command. Nothing updates on its own.

## When something doesn't work

- `sudo docker compose logs --tail 100 uwumail` shows what the server is doing.
  On the gateway VPS: `sudo journalctl -u uwumail-gateway`.
- The admin overview in the portal shows the health of DNS, certificate,
  sending, storage, logins and the gateway; *Server → Setup* runs the checks
  again.
- **No certificate:** `mail.example.com` must point to the server (A) or the
  gateway (B), and port 80 must be reachable.
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
