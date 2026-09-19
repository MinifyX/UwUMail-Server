# Installing UwUMail Server

This is how to set up UwUMail Server from nothing to the first mail in your
inbox. Plan an hour, most of it waiting for DNS.

> UwUMail is a just-for-fun project that I build for myself, and I run my own
> mail on it. It is still young: set up backups, and don't expect support. Issues
> are okay, but I might answer late or not at all.

The examples are an Ubuntu 26.04 machine and a domain `example.com` whose mail
server is called `mail.example.com`. Ubuntu 24.04 and other Linux distributions
with Docker work the same; only the `apt` lines differ.

## Which of the four ways is yours

Two questions decide it: does the machine face the internet itself, and is
anything else on it already using the ports UwUMail wants?

| | Nothing else on the machine | Other containers already there |
| --- | --- | --- |
| **Straight to the internet** | [1. A machine of its own](#1-a-machine-of-its-own) | [2. Next to other containers](#2-next-to-other-containers) |
| **Through a gateway on its own VPS** | [3. At home behind a gateway](#3-at-home-behind-a-gateway) | [4. Behind a gateway, next to other containers](#4-behind-a-gateway-next-to-other-containers) |

**Straight to the internet** means the machine has a fixed public IPv4 address
whose reverse DNS (PTR) you can set, and port 25 is open in both directions.
That is the normal case for a rented VPS or root server. At home it needs a
business line and port forwarding on the router —
[the router part](#port-forwarding-on-your-router) of this page walks through
Speedport, FRITZ!Box, UniFi and others.

**Through a gateway** means a small VPS runs the
[UwUMail Gateway](gateway.md) and your server dials out to it. Nothing has to be
opened on the router, it works behind carrier-grade NAT and DS-Lite, and other
mail servers only ever see the gateway's address. This is the way for a normal
home connection: those hand out changing addresses that Spamhaus lists in the
PBL on purpose, their reverse DNS is made up by the provider, and many
providers block port 25 outright. A Raspberry Pi is enough for the server.

**Other containers already there** only changes the ports. UwUMail wants TCP
25, 80, 443, 465, 587 and 993. Ways 2 and 4 are ways 1 and 3 plus the steps that
keep things out of each other's way.

The other pages call ways 1 and 2 *setup A*, and ways 3 and 4 *setup B*.

## What you need in every case

- **A domain** where you can edit DNS records. The server gets a name in it,
  for example `mail.example.com`.
- **A Linux machine for the server**, amd64 or arm64, 1 GB RAM or more (2.5 GB
  or more for the [virus scanner](antivirus.md), which the installer then brings
  along), with Docker.
- **Ways 1 and 2:** a fixed public IPv4 address (IPv6 as well is better), port
  25 open in both directions, and reverse DNS you can set.
- **Ways 3 and 4:** a small VPS for the gateway, with the same three things.
  The smallest plan of most providers is plenty; the ready-made gateway is
  built for amd64. Avoid Hetzner Cloud for it, see
  [gateway.md](gateway.md#what-you-need).

---

## 1. A machine of its own

A fresh Ubuntu 26.04 machine, nothing else on it, facing the internet itself.

### 1.1 Install Docker

```bash
sudo apt update
sudo apt install -y docker.io docker-compose-v2
docker compose version
```

`docker compose version` has to answer with version 2 or newer. On other
systems, follow [Docker's guide](https://docs.docker.com/engine/install/). The
installer stops early and says so if Docker is missing or not running.

### 1.2 Open the ports

UwUMail wants these, all TCP:

| Port | What for |
| --- | --- |
| 25 | mail from other servers |
| 80 | certificate challenges, redirect to HTTPS |
| 443 | the portal, JMAP, and the apps |
| 465 | mail apps, sending over TLS |
| 587 | mail apps, sending with STARTTLS |
| 993 | mail apps, reading over TLS |

**On a VPS or root server:** open those six in the provider's firewall, and
check that port 25 is open outgoing as well. Many providers block it for new
customers until you ask. The setup assistant tests it later and says what it
found.

**On a machine at home:** forward the same six ports on the router to this
machine, and give the machine a fixed address in your network first. The
[router part](#port-forwarding-on-your-router) has the click paths. If your
connection has no public IPv4 address of its own (DS-Lite, carrier-grade NAT),
forwarding cannot work at all — that is what way 3 is for.

A word on `ufw`: Docker publishes container ports past it, so a rule there
neither opens nor closes what Compose publishes. Keep `ufw` for SSH and don't
expect it to guard the mail ports.

### 1.3 DNS and reverse DNS

Create these before the server starts, so the certificate works at the first
try:

| Record | Value |
| --- | --- |
| `mail.example.com A` | the machine's public IPv4 address |
| `mail.example.com AAAA` (if you have IPv6) | its IPv6 address |

Then set the **reverse DNS** (PTR) of those addresses to `mail.example.com`, in
your provider's panel. Without it, big mail providers refuse your mail with
answers like `554 Invalid DNS PTR resource record`.

MX, SPF, DKIM and DMARC come in [1.6](#16-setup-assistant); the assistant shows
them with the right values.

### 1.4 Run the installer

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo bash install.sh
```

It asks what it needs to know and does the rest: it sets up `/opt/uwumail` with
`compose.yaml`, an `.env` and `update.sh`, starts the server and shows the
**one-time code** for the setup assistant. The questions are:

| It asks | What to answer here |
| --- | --- |
| Public name of this mail server | `mail.example.com` from [1.3](#13-dns-and-reverse-dns) |
| E-mail for certificate warnings | Optional. Let's Encrypt warns you there before a certificate expires |
| Language of the mail the server writes | `de` or `en`, for bounces and notices |
| Behind a UwUMail Gateway? | No — that is ways 3 and 4 |
| Virus scanner | ClamAV, about 1 GB of memory. On by default, and skipped on a machine with less than 2.5 GB. See [antivirus.md](antivirus.md) |
| System updates in the portal | Installs a small helper, so the portal can show and install what the machine itself needs |

Every answer is a flag as well, so nothing has to be typed:

```bash
sudo bash install.sh --hostname mail.example.com --email you@example.org \
  --language en --yes
```

`sudo bash install.sh --help` lists them all, among them `--dir` for a place
other than `/opt/uwumail`, `--version` for a tag other than `latest`, and
`--no-host-helper`.

### 1.5 The one-time code

The installer prints it when the server answers its health check. It is good
until the first admin account exists, and the server makes a new one at every
start, so the newest one in the log is the one that works:

```bash
cd /opt/uwumail && sudo docker compose logs uwumail | grep "one-time code"
```

### 1.6 Setup assistant

Open `https://mail.example.com/setup` and enter the code.

If the name doesn't resolve yet, `https://<the machine's address>/setup` works
too; the browser warns about the certificate. Use that only to get set up:
passkeys and the Apple profile only work with `https://mail.example.com`, and
invitation links you copy in the portal carry the address you opened it with.

The assistant

1. creates your admin account and your first domain,
2. checks how the server is connected (*Reachability*) and recommends a gateway
   or sending directly — here it should say sending directly is fine,
3. shows the DNS records for the domain and checks them; for a domain at
   Cloudflare it can add them with an API token that is not stored,
4. checks whether mail gets out and whether port 25 answers,
5. checks reverse DNS and, only when you ask, the blocklists,
6. sends a test mail, optionally also to another address of yours. Reply to it
   from there: when the reply arrives, mail from outside works.

The certificate comes by itself; the log says `got a fresh certificate` when it
is there. The server asks Let's Encrypt when it starts, so if
`mail.example.com` didn't reach it on port 80 then, it tries again within the
hour — `sudo docker compose restart uwumail` makes it try right away.

Everything the assistant checked stays in the portal under *Server → Setup*.
More domains and people are added under *Domains* and *Accounts*.

### 1.7 Mail apps

- **UwUMail apps** only need `https://mail.example.com`.
- **Thunderbird, Outlook and most others** find the settings themselves from
  the address, once `autoconfig.example.com` and `autodiscover.example.com`
  point to the same address as `mail.example.com`.
- **iPhone, iPad and Mac:** *My account → Connect mail apps* makes a profile
  with mail, calendars and contacts.
- **Everything else:** IMAP on 993 (TLS), sending on 465 (TLS) or 587
  (STARTTLS), all at `mail.example.com`, logging in with the full address.

Details, calendars and contacts: [deployment.md](deployment.md#mail-apps).

### 1.8 Backups

Set them up before you rely on the server: *Server → Backups* backs up every
night to an SFTP server such as a NAS, deduplicated and encrypted. Keep the
recovery key somewhere else than the server. See [backups.md](backups.md).

Putting one back is on the same page, beside the snapshot. On a machine that
has no server yet, the setup assistant offers it instead of creating the first
admin — which is what you want when this machine stands in for one that died.

### 1.9 Virus scanner, if you skipped it

The installer brings ClamAV along unless you said no, or unless the machine has
less than 2.5 GB of memory — it wants about a gigabyte for itself. *Spam filter
→ Viruses* in the portal shows whether it is there, and what it turned away.

Adding it later, in `/opt/uwumail`:

```bash
sudo docker compose --profile antivirus up -d
sudo docker compose exec uwumail uwumail-server settings set spam.antivirus.enabled true
```

and `COMPOSE_PROFILES=antivirus` in `.env`, so it comes along at every start
from then on. `update.sh` offers the same thing when it is not there yet. Its
first start takes a few minutes while it fetches its signatures. Everything
about it: [antivirus.md](antivirus.md).

That is the whole way. [Updates](#keeping-it-up-to-date) are `update.sh` in
`/opt/uwumail`.

---

## 2. Next to other containers

The same machine as in way 1, but Docker already runs something — a reverse
proxy, a web app, maybe another mail server. Only the ports change; everything
else is way 1.

### 2.1 Find out what is taken

```bash
sudo ss -tulpn | grep -E ':(25|80|443|465|587|993)\b'
sudo docker ps --format 'table {{.Names}}\t{{.Ports}}'
```

Anything answering on one of the six ports has to be dealt with before the
first start, or Docker refuses it with `address already in use` or
`port is already allocated`. Docker itself is installed already; if not,
[1.1](#11-install-docker).

Sort the finds into two groups. **The web ports 80 and 443** are the common
case and have a ready-made answer. **The mail ports 25, 465, 587 and 993** are
rarer and usually mean another mail server, which needs a decision.

### 2.2 Web ports 80 and 443 are taken

A reverse proxy (Caddy, Traefik, nginx, Nginx Proxy Manager) holds them. It
keeps them, and it passes `mail.example.com` on to UwUMail — that part is not
optional here: in way 2 the certificate challenge and the portal both arrive
through it.

The mail ports are never proxied. UwUMail keeps 25, 465, 587 and 993 itself.

Two lines move UwUMail's own web ports out of the way, as a port or as
`address:port`:

```bash
UWUMAIL_HTTP_BIND=127.0.0.1:8081
UWUMAIL_HTTPS_BIND=8443
```

They belong in `/opt/uwumail/.env`, which the installer writes in
[2.5](#25-run-the-installer) — that step says how to have them in place for the
very first start.

Then give the proxy UwUMail's **proxy listener** on port 8080, which serves the
whole site over plain HTTP. For a proxy that runs in Docker on the same
machine, the repository has the files:
[`deploy/behind-proxy`](../deploy/behind-proxy). Copy `compose.proxy.yaml` next
to `compose.yaml` and add to `.env`:

```bash
COMPOSE_FILE=compose.yaml:compose.proxy.yaml
```

It switches the proxy listener on, trusts the proxy's address and creates the
Docker network `uwumail-proxy`, which the proxy joins to reach `uwumail:8080`.
The header of that file has the steps, and the `Caddyfile` next to it is
Caddy's side. For a proxy on the host instead of in Docker, the end of the same
file has the variant.

Two things that cost people an evening:

- **Never point a proxy at UwUMail's port 80.** That port only redirects to
  HTTPS, and the browser ends up in a loop. The upstream is 8080.
- **The proxy has to pass `Host` on and set `X-Forwarded-For` and
  `X-Forwarded-Proto`**, and its address belongs in `http.trusted_proxies`.
  While that list is missing the proxy, every visitor counts as the proxy: all
  logins share one throttle and the logs show one address.

Route `mta-sts.example.com`, `autoconfig.example.com` and
`autodiscover.example.com` to UwUMail as well, once their records exist. The
whole story: [deployment.md](deployment.md#behind-a-reverse-proxy).

### 2.3 Mail ports are taken

**Another mail server has port 25.** Two ways out:

- *Let it stay in front.* It keeps port 25 and hands one (sub)domain to
  UwUMail, which is how I tried UwUMail beside Mailcow. The five steps and
  ready-made files:
  [deployment.md → Next to an existing mail server](deployment.md#next-to-an-existing-mail-server)
  and [`deploy/next-to-mailserver`](../deploy/next-to-mailserver). Coming over
  from Mailcow for good:
  [migrating-from-mailcow.md](migrating-from-mailcow.md).
- *Move it off the machine or switch it off*, and follow way 1 from here.

**Something that is not a mail server sits on 465, 587 or 993** — that happens.
Move that service, or let UwUMail publish the port somewhere else and have the
router land on it.

The mail ports in the stock `compose.yaml` are fixed, and an override file that
simply lists `ports:` makes it worse: Compose merges the two lists, so the old
entry stays and takes the conflict with it. The `!override` tag replaces the
list instead — Docker Compose 2.24.4 or newer, which `docker compose version`
confirms. Next to `compose.yaml`, as `compose.ports.yaml`:

```yaml
services:
  uwumail:
    ports: !override
      - "25:25"
      - "${UWUMAIL_HTTP_BIND:-80}:80"
      - "${UWUMAIL_HTTPS_BIND:-443}:443"
      - "1465:465"      # the machine answers on 1465, the router forwards 465 here
      - "993:993"
      - "587:587"
```

In `.env`, with `compose.proxy.yaml` from [2.2](#22-web-ports-80-and-443-are-taken)
in the list too if you use it:

```bash
COMPOSE_FILE=compose.yaml:compose.ports.yaml
```

`sudo docker compose config` prints what came out of both files before anything
starts. Editing `compose.yaml` itself would do the same job, but then every
`update.sh` has to ask about it; an override file leaves the stock file alone
and is carried along by name.

Port 25 is the one exception: from the outside it has to arrive on **25**,
because that is the only port other mail servers ever try. On a machine at home
the router can do the translating (external 25 → internal 1025); on a VPS it
cannot.

### 2.4 DNS and reverse DNS

Exactly as in way 1: the [A, AAAA and PTR records](#13-dns-and-reverse-dns),
and the six ports open ([1.2](#12-open-the-ports)) — with the moved ones
instead, where you moved them. At home, the
[router part](#port-forwarding-on-your-router) has the click paths, including
how to forward an external port to a different internal one.

### 2.5 Run the installer

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo env UWUMAIL_HTTP_BIND=127.0.0.1:8081 UWUMAIL_HTTPS_BIND=8443 bash install.sh
```

The two variables are for this one run: the installer starts the server at the
end, and without them that start walks into the occupied ports and stops with
`address already in use`. Compose takes them from the environment, ahead of
anything in `.env`. It asks the same questions as in
[1.4](#14-run-the-installer); the gateway question is a no here.

Nothing bad happens if you forget them — the installer has written
`/opt/uwumail` by then, and only the start failed. Either way the two lines
have to end up in `.env`, or the next `up -d` takes port 80 again:

```bash
cd /opt/uwumail
sudo nano .env      # the two UWUMAIL_..._BIND lines, and COMPOSE_FILE if you use one
sudo docker compose up -d
sudo docker compose logs uwumail | grep "one-time code"
```

### 2.6 Setup assistant

As in [1.6](#16-setup-assistant), at `https://mail.example.com/setup` through
the proxy. Two differences:

- While the proxy does not pass the name on yet, the way in is
  `https://<the machine's address>:8443/setup`, with the certificate warning.
- UwUMail asked Let's Encrypt for its certificate a few seconds after the
  start, before the proxy knew the name, so that first try failed. Once the
  site opens through the proxy: `sudo docker compose restart uwumail`. That
  makes a new one-time code, so read it from the log again.

### 2.7 The rest

[Mail apps](#17-mail-apps), [backups](#18-backups) and the
[virus scanner](#19-virus-scanner-if-you-skipped-it) are the same as in way 1.

---

## 3. At home behind a gateway

A fresh Ubuntu 26.04 machine at home, nothing else on it, and a small VPS that
runs the [UwUMail Gateway](gateway.md) and nothing else. The server dials out
to the gateway, so the router stays closed.

### 3.1 Rent the VPS

Debian 12 or newer, Ubuntu 24.04 or newer, amd64, an image without Plesk or
anything else that wants the same ports. It needs a fixed public IPv4 address
(IPv6 as well is better), reverse DNS you can set, and outgoing port 25.

The VPS is the gateway's alone. It wants ports 25, 80, 443, 465, 587 and 993
for itself, and whatever else you put there shares its fate. Hetzner Cloud
blocks ports 25 and 465 and only unblocks them case by case after a month as a
customer, so pick another provider; on the STRATO VPS I tried in September 2026
port 25 was open from the first minute.

Open on the VPS, in the provider's firewall: **TCP 25, 80, 443, 465, 587 and
993, and UDP 443** for the tunnel.

### 3.2 Install the gateway

The gateway comes first, because the server pairs with it when it starts. On
the VPS:

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz.sha256
sha256sum -c uwumail-gateway-linux-amd64.tar.gz.sha256
tar -xzf uwumail-gateway-linux-amd64.tar.gz
sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway
```

It installs the gateway as a systemd service, looks after the machine itself
(unattended upgrades, fail2ban, a firewall, a hardened SSH configuration) and
prints a **pairing code** (`uwugw1…`). Keep it for [3.5](#35-run-the-installer);
`sudo uwumail-gateway code` shows it again. What it changes and what it sees:
[gateway.md](gateway.md#what-it-does-to-the-machine).

### 3.3 DNS and reverse DNS

Everything points at the **gateway**, never at your home connection:

| Record | Value |
| --- | --- |
| `mail.example.com A` | the gateway's IPv4 address |
| `mail.example.com AAAA` (if it has IPv6) | the gateway's IPv6 address |

Set the reverse DNS (PTR) of the gateway's addresses to `mail.example.com`, at
the VPS provider.

Nothing in DNS mentions your home address, and nothing has to. MX, SPF, DKIM
and DMARC come in [3.7](#37-setup-assistant).

### 3.4 Install Docker at home

```bash
sudo apt update
sudo apt install -y docker.io docker-compose-v2
docker compose version
```

Nothing has to be opened on the router. The server only needs to reach the
gateway on **UDP port 443** outgoing, which home routers allow by default.

### 3.5 Run the installer

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo bash install.sh
```

Same questions as in [1.4](#14-run-the-installer), with one answered
differently: *Does this server run at home behind a UwUMail Gateway?* is a yes,
and then it wants the pairing code from [3.2](#32-install-the-gateway). As a
flag it is `--gateway-code uwugw1…`.

The code goes in before the first start on purpose. The host name already
points at the gateway, and an unpaired gateway closes port 443, so without the
code there is no way into the setup assistant by name.

### 3.6 Watch the tunnel come up

```bash
cd /opt/uwumail
sudo docker compose logs uwumail | grep -E "paired with|connected to the UwUMail Gateway|got a fresh certificate"
```

In that order: the server pairs, the tunnel comes up, and then the server asks
Let's Encrypt — the challenge arrives through the tunnel, so no restart is
needed. The installer has already printed the one-time code; if it is gone,
`sudo docker compose logs uwumail | grep "one-time code"`.

### 3.7 Setup assistant

Open `https://mail.example.com/setup`; it works through the gateway.
`https://<the machine's address in your network>/setup` works as well, with the
certificate warning.

The assistant does the same six things as in [1.6](#16-setup-assistant), with
one difference: under *Reachability* the gateway already shows as paired. If
you left the code out, choose *Through a gateway* and paste it here instead.

From then on, mail to other servers only leaves through the gateway, also while
it is unreachable — it waits in the queue instead of leaking your home address.

### 3.8 Mail apps, backups

[Mail apps](#17-mail-apps), [backups](#18-backups) and the
[virus scanner](#19-virus-scanner-if-you-skipped-it) as in way 1, with one
thing to know: a local DNS entry that points `mail.example.com` at the home
machine saves apps at home the detour over the VPS, but it moves the whole
name, so the machine then has to answer on 443, 993, 465 and 587 itself. The
simplest is no local entry — use the gateway from home too.

---

## 4. Behind a gateway, next to other containers

Way 3 on a machine where Docker already runs other things. This is the friendly
combination: because everything arrives through the tunnel, the machine's own
ports don't have to be reachable from anywhere, and moving them collides with
nothing.

### 4.1 Find out what is taken

```bash
sudo ss -tulpn | grep -E ':(25|80|443|465|587|993)\b'
sudo docker ps --format 'table {{.Names}}\t{{.Ports}}'
```

The web ports 80 and 443 are the usual finds, and here they are the easy case.
The mail ports are only in the way when another mail server sits on the
machine, and that one needs the same decision as in
[2.3](#23-mail-ports-are-taken) — with a gateway in front the two arrangements
do not mix, so sort that out first.

### 4.2 The VPS, the gateway, DNS

Unchanged from way 3: [rent the VPS](#31-rent-the-vps),
[install the gateway](#32-install-the-gateway), and point
[DNS and reverse DNS](#33-dns-and-reverse-dns) at it. The other containers at
home are none of the gateway's business.

### 4.3 Move UwUMail's web ports, and nothing else

```bash
UWUMAIL_HTTP_BIND=127.0.0.1:8081
UWUMAIL_HTTPS_BIND=8443
```

That is all. **No reverse proxy entry, no `compose.proxy.yaml`**: the web
arrives through the tunnel, and the other web server is not in the public path
at all. The two lines only keep the container from colliding with it locally.

`https://<the machine's address>:8443` then reaches the portal without the
gateway; the browser warns about the certificate. That is a fallback for setup
and admin work, not an address to hand out — passkeys are bound to
`https://mail.example.com` without a port.

An entry for the name in the local proxy is only of use together with a local
DNS entry pointing the name at this machine, and the proxy cannot get a public
certificate for it, because Let's Encrypt's checks end at the gateway. The
reasons and the two ways around it:
[deployment.md](deployment.md#with-a-gateway-setup-b).

### 4.4 Install Docker, run the installer

```bash
sudo apt update
sudo apt install -y docker.io docker-compose-v2
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo env UWUMAIL_HTTP_BIND=127.0.0.1:8081 UWUMAIL_HTTPS_BIND=8443 bash install.sh
```

The two variables are for this one run, so that the start at the end does not
walk into the occupied ports; Compose takes them from the environment. The
questions are the ones from [1.4](#14-run-the-installer), with yes to the
gateway and the pairing code from [3.2](#32-install-the-gateway).

Afterwards the two lines have to end up in `.env` as well, or the next `up -d`
takes port 80 again:

```bash
cd /opt/uwumail
sudo nano .env
sudo docker compose up -d
```

### 4.5 The rest

[Watch the tunnel come up](#36-watch-the-tunnel-come-up) and the
[setup assistant](#37-setup-assistant) as in way 3 — through the gateway, or at
`https://<the machine's address>:8443/setup` — then
[mail apps](#17-mail-apps), [backups](#18-backups) and the
[virus scanner](#19-virus-scanner-if-you-skipped-it).

---

## Port forwarding on your router

Only for ways 1 and 2, and only when the machine stands at home. With a gateway
(ways 3 and 4) the server dials out and none of this applies; on a VPS the
provider's firewall does the same job with less clicking.

Be honest with yourself before you start. A normal consumer line gives you a
changing address that Spamhaus lists in the PBL on purpose, reverse DNS that
the provider made up and won't change, and quite often a blocked port 25. Big
providers turn that mail away no matter how right your DNS is. Port forwarding
is for a business line with a fixed address and a PTR entry you can have set.
For everything else, way 3 exists.

### What has to be forwarded

Six rules, all TCP, all to the same machine:

| External port | To the machine, port | What for |
| --- | --- | --- |
| 25 | 25 | mail from other servers |
| 80 | 80 | certificate challenges |
| 443 | 443 | portal, JMAP, apps |
| 465 | 465 | mail apps, TLS |
| 587 | 587 | mail apps, STARTTLS |
| 993 | 993 | mail apps, IMAP |

Give the machine a **fixed address in your network** first — a DHCP reservation
in the router, or a static address on the machine. A rule points at an address,
and a machine that gets a new one after a reboot takes the mail with it.

External and internal port may differ, which is the way out when something else
on the machine already has one of them ([2.3](#23-mail-ports-are-taken)). Port
25 is the exception: other mail servers only ever try 25, so on the outside it
has to be 25 — but external 25 → internal 1025 is fine, if that is what the
machine listens on.

**IPv6 has no forwarding.** There is nothing to translate; instead the router's
firewall has to let those ports through to the machine's IPv6 address. Every
interface below calls that something different, and some hide it until IPv6 is
switched on.

### FRITZ!Box

FRITZ!OS 7 and 8, at `http://fritz.box`:

1. **Heimnetz → Netzwerk**, the machine's entry, pencil icon: tick *Diesem
   Netzwerkgerät immer die gleiche IPv4-Adresse zuweisen*.
2. **Internet → Freigaben → Portfreigaben**, then *Gerät für Freigaben
   hinzufügen*, and pick the machine under *Gerät*.
3. *Neue Freigabe* → *Portfreigabe*. Under *Anwendung* pick *Andere
   Anwendung*, give it a name (`UwUMail SMTP`), *Protokoll* `TCP`, then *Port
   an Gerät* `25`, *bis Port* `25` and *Port extern gewünscht* `25`.
4. Repeat for 80, 443, 465, 587 and 993, then **OK** and **Übernehmen**.

The same dialog holds the IPv6 rules once IPv6 is on under *Internet →
Zugangsdaten → IPv6*; they are separate entries with the same ports. A changing
address is handled under *Internet → Freigaben → DynDNS* or with MyFRITZ!, but
a DynDNS name fixes neither the PBL nor the missing PTR — read the warning
above.

### Telekom Speedport

Speedport Smart 3 and 4, at `http://speedport.ip`:

1. Log in with the device password (on the sticker), then switch *Ansicht* from
   *Standard* to *Experte*; the port entries are hidden otherwise.
2. **Heimnetzwerk → Geräte**, the machine, and give it the same address
   permanently.
3. **Internet → Portfreischaltung**, pick the machine, then *Port-Weiterleitung*
   (same port inside and outside) or *Port-Umleitung* (a different port inside).
4. Protocol `TCP`, the port, save. Repeat for all six.

Telekom lines are dual-stack, so the IPv4 rules do work; the Speedport keeps
the IPv6 ones in the same place. Port 25 is what to check first: outgoing SMTP
is filtered on many consumer lines.

### UniFi

UniFi Network, at the UniFi OS console, the app or `unifi.ui.com`. The entry
moved more than once, so look for *Port Forwarding* if none of these match your
version:

- **Network 10:** *Settings → Policy Engine → Policy Table → Create New
  Policy → Port Forwarding*
- **Network 9:** *Settings → Routing → Port Forwarding*
- **Network 8 and older:** *Settings → Security → Port Forwarding*

Before that, give the machine a fixed address: *Client Devices*, the machine,
*Settings → Fixed IP*.

The rule's fields are the same everywhere: *Name*, *WAN Interface* (which
connection, when there are two), *WAN Port* (25), *From* (`Any` — mail comes
from everywhere, so this one cannot be limited), *Forward IP Address* (the
machine), *Forward Port* (25) and *Protocol* (`TCP`). Six rules.

IPv6 is not port forwarding here either: it needs a firewall rule that allows
*Internet In* to the machine's address on those ports.

### OPNsense and pfSense

*Firewall → NAT → Port Forward*, one rule per port: interface `WAN`, protocol
`TCP`, destination *WAN address*, destination port from the table, redirect
target the machine's address and the same port. Leave *Filter rule association*
at *Add associated filter rule*, or the packets are translated and then
dropped.

For IPv6, add rules under *Firewall → Rules → WAN* that allow those ports to
the machine's address; no NAT.

### Any other router

The wording differs, the idea doesn't. Look for *Port forwarding*,
*Portfreigabe*, *Portfreischaltung*, *Virtual Server*, *NAT* or
*Applications & Gaming*, and enter protocol, external port, the machine's
address and its port. Two that come up often:

- **Vodafone Station and other cable routers:** the entries only work with a
  public IPv4 address. On a DS-Lite line there is none, and the menu is hidden
  or useless. Vodafone hands out a public IPv4 on request for some contracts;
  otherwise it is way 3.
- **A router whose own web interface sits on port 80 or 443:** switch its
  remote access off, or it keeps the port for itself and the rule never fires.

### When it looks right and still doesn't work

- **No public IPv4 address** (DS-Lite, carrier-grade NAT): nothing arrives, no
  matter what the rules say. `curl -s https://ifconfig.me` on the machine and
  the WAN address the router shows have to be the same one. A WAN address
  between `100.64.` and `100.127.` is carrier-grade NAT. → way 3.
- **The setup assistant says port 25 doesn't answer, but mail arrives.** The
  check calls the server on its own public address, and many routers cannot
  reach themselves that way (no hairpin NAT). The reply to the test mail is the
  reliable answer.
- **Outgoing port 25 is blocked.** Consumer lines often filter it. Until the
  provider opens it, the server can send through a relay on port 587 — the
  setup assistant offers that under *Reachability*.
- **The address changes every night.** DynDNS keeps the name pointing at you,
  but the new address has no PTR and may be listed. This is the case the
  gateway was written for.

---

## Keeping it up to date

*Server → Updates* in the portal shows when a new version is out and what
changed. The update itself happens on the machine:

```bash
cd /opt/uwumail && sudo bash update.sh
```

It fetches a newer `update.sh` first and hands over to it, backs up if a backup
server is set up, brings `compose.yaml` up to date, pulls the images and waits
for the server to answer its health check. If it does not, the version from
before comes back and `.env` says which one that is. Mail stays where it is,
and database changes run by themselves.

A server set up before 0.4.0 has no `update.sh` yet, so it gets one once:

```bash
cd /opt/uwumail
sudo curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/update.sh
sudo bash update.sh
```

`sudo bash update.sh --help` lists every flag, among them `--no-backup`,
`--version` for another tag, and `--keep-compose`.

**Ways 2 and 4:** a `compose.yaml` you edited is not walked over. What it can,
`update.sh` moves into `.env` — a changed web port, a pinned image tag, the
virus scanner — and anything else stops it with a diff and one sentence about
`--force`. An override file next to it
([2.3](#23-mail-ports-are-taken)) is the quieter way: the stock `compose.yaml`
stays the stock one, `COMPOSE_FILE` in `.env` keeps loading yours on top, and
the update has nothing to ask about.

**The gateway** (ways 3 and 4) is updated with the same commands that installed
it; the portal shows them with the exact version.

### Buttons for the machine itself (optional)

The server's container cannot touch the machine it runs on. It is distroless,
read-only, unprivileged, and every capability is dropped but the one it needs
for the low mail ports — which is most of what makes a break-in worth little,
so it stays that way.

A small helper beside it can, and then the portal shows what the system has
waiting and installs it with a button. The installer offers it; adding it
later:

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-host.tar.gz
tar -xzf uwumail-host.tar.gz
sudo bash uwumail-host/install.sh --dir /opt/uwumail
cd /opt/uwumail && sudo docker compose up -d
```

What the container may ask the helper for is one of two things: install the
system's updates, or restart the machine. Never a command, never a path, never
an address, and never a version of anything — UwUMail itself is updated by
`update.sh`, which a person starts. The docker socket stays where it is:
handing that to a container is handing it the machine.

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
- **`address already in use` or `port is already allocated`:** another program
  has one of the six ports. Which one is in the message;
  [2.1](#21-find-out-what-is-taken) finds out who has it, and
  [2.2](#22-web-ports-80-and-443-are-taken) and
  [2.3](#23-mail-ports-are-taken) move it out of the way.
- **The installer stops with `/opt/uwumail is already set up`:** that is the
  guard against a second first-time install. To go on from what is there:
  `cd /opt/uwumail && sudo bash update.sh`, or edit `.env` and
  `sudo docker compose up -d`.
- **A line in `.env` changes nothing:** `UWUMAIL_GATEWAY_CODE`,
  `UWUMAIL_HTTP_BIND` and `UWUMAIL_HTTPS_BIND` only work with a `compose.yaml`
  that mentions them; one from before they existed ignores them without a word.
  `grep -c UWUMAIL_HTTP_BIND compose.yaml` has to answer 1 or more. If it
  doesn't, `sudo bash update.sh` brings the file up to date, keeping what you
  changed in it. `.env` and your mail stay.
- **The browser says too many redirects:** a reverse proxy points at UwUMail's
  port 80, which only redirects to HTTPS. Point it at the proxy listener on
  8080, see [deployment.md](deployment.md#behind-a-reverse-proxy). In ways 3
  and 4, remove the entry from the proxy instead; it is not needed.
- **Ways 3 and 4: `https://mail.example.com` doesn't open:** the gateway passes
  the web on only while the server is connected, so the log has to show
  `connected to the UwUMail Gateway`. If it says nothing about the gateway, the
  code didn't reach the server: it is missing in `.env`, or `compose.yaml` is an
  older one (see above). If it says `could not reach the UwUMail Gateway`, check
  that UDP port 443 is open on the VPS. After a change in `.env`,
  `sudo docker compose up -d` applies it; a restart does not.
- **Ways 3 and 4: the log says `the UwUMail Gateway refused this server`:** the
  gateway is paired with another key. *Forget gateway* in the portal and a new
  data volume both make a new key. Run `sudo uwumail-gateway unpair` on the VPS
  and use the new code. A code in `.env` wins over a pairing made in the portal
  at the next start, so don't leave an old one there.
- **No certificate:** `mail.example.com` must point to the machine (ways 1 and
  2) or the gateway (ways 3 and 4). In ways 1 and 2, port 80 must be reachable
  from outside; in ways 3 and 4, the tunnel must be connected. The server tries
  again within the hour.
- **Mail doesn't go out:** port 25 blocked by the provider, or reverse DNS
  missing. *Server → Queue* shows why a message is still waiting.
- **Mail doesn't come in:** check the MX record, and in ways 1 and 2 that port
  25 arrives from the outside — not only from your own network.

## More

- [deployment.md](deployment.md): every DNS record, MTA-STS, reverse proxy,
  running next to an existing mail server
- [configuration.md](configuration.md): all settings
- [gateway.md](gateway.md): how the gateway works
- [spam-filter.md](spam-filter.md): how the spam filter decides
- [antivirus.md](antivirus.md): the virus scanner beside the server
- [backups.md](backups.md): backups, and putting one back
- [migrating-from-mailcow.md](migrating-from-mailcow.md): moving over from mailcow
