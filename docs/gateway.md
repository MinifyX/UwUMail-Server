# UwUMail Gateway

Running a mail server at home is fun until other mail servers refuse to talk to
it: home connections have changing addresses that sit on blocklists (Spamhaus
PBL lists them on purpose), providers block port 25, reverse DNS can't be set,
and behind carrier-grade NAT nothing reaches you at all.

The UwUMail Gateway fixes that with a tiny rented server (a VPS) that has a
fixed public address. It takes all mail, mail app and web connections and
carries them through a tunnel to your UwUMail server at home, and it sends your
outgoing mail from its own address. Other servers, DNS records and mail headers
only ever show the gateway, never your home address.

> Early and just for fun, like the rest of UwUMail. I run it for my own test
> instance.

## How it works

```
                                   VPS                                  at home
 other mail servers ──▶ ┌──────────────────────┐              ┌──────────────────────┐
 mail apps          ──▶ │   UwUMail Gateway    │  QUIC tunnel │    UwUMail Server    │
 browsers           ──▶ │ :25 :465 :587 :993   ◀════════════════ dials out (UDP 443) │
                        │ :80 :443             │              │                      │
                        │                      │              │                      │
 other mail servers ◀── │ outgoing mail leaves │ ◀─ "connect" │ queue, DKIM, TLS     │
                        │ from the VPS address │              │ and all data         │
                        └──────────────────────┘              └──────────────────────┘
```

- **The server dials out.** No port forwarding on your router, and it works
  behind carrier-grade NAT or DS-Lite. The connection is QUIC with a certificate
  on each side; each side pins the other's fingerprint.
- **TLS ends at home.** The gateway passes bytes along. It never sees passwords
  or the content of TLS connections (ports 465, 993 and 443, and port 25 and 587
  after STARTTLS); the certificate and its key stay on your server.
- **Real client addresses.** Each carried connection starts with the client's
  address, so SPF checks, login limits and logs work as if the server stood on
  the internet itself.
- **No mail on the VPS.** While your server is away (power cut, update, the
  daily reconnect of the home connection), the gateway answers other mail
  servers with `421 try again later`. They retry for days, as SMTP intends.
- **Outgoing mail stays yours.** Queue, retries and DKIM signing run at home.
  The gateway only opens the connection to the other server, and only to mail
  ports (25, 465, 587) of public addresses, so nobody can use it for anything
  else.

## What you need

- **A small VPS.** The gateway is a single program that needs a few megabytes
  of memory; the smallest plan of most providers is plenty. Linux with systemd
  (Debian 12 or newer, Ubuntu 24.04 or newer), no other mail or web server on it
  (pick an image without Plesk or similar).
- **A fixed IPv4 address** (IPv6 as well is better) with **reverse DNS you can
  set**. Set it before you send the first mail: a gateway whose address has no
  matching reverse entry is turned away by big mail providers outright, with
  answers like `554 Invalid DNS PTR resource record`.
- **Outgoing port 25.** Many providers block it for new servers and open it
  when you ask, so check before you rent — but check for real rather than
  trusting a table: on the STRATO VPS I tried in September 2026 (its network
  belongs to IONOS, AS8560) port 25 was open from the first minute. The setup
  assistant tests the port itself and only points at the provider when the
  port really is closed.

  One provider is worth avoiding for this job: Hetzner Cloud blocks ports 25
  and 465, and you can ask to unblock them only after a month as a customer
  and a paid first invoice, decided case by case
  ([Hetzner FAQ](https://docs.hetzner.com/cloud/servers/faq/)).

  Until port 25 is open, the server can send through a relay on port 587; that
  connection goes through the gateway too.
- **Open ports on the VPS:** TCP 25, 80, 443, 465, 587 and 993 from everywhere, and
  UDP 443 for the tunnel.
- **At home:** your UwUMail server may send UDP to the VPS.

## Install the gateway

Every release carries a ready gateway for amd64. On the VPS:

```bash
cd /tmp
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/uwumail-gateway-linux-amd64.tar.gz.sha256
sha256sum -c uwumail-gateway-linux-amd64.tar.gz.sha256
tar -xzf uwumail-gateway-linux-amd64.tar.gz
sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway
```

The same commands update it; when a new release is out, the portal shows them
with the exact version.

To follow `main` instead, from a clone of this repository, with the GitHub CLI
(`gh`, logged in, because CI artifacts need a login) and SSH access to the VPS:

```bash
UWUMAIL_GATEWAY_HOST=root@203.0.113.10 scripts/deploy-gateway.sh
```

It takes the gateway that CI built from the newest successful commit on `main`
and checks its SHA-256 sum (`UWUMAIL_GATEWAY_RUN=<run id>` picks another run).
CI builds for amd64 only; for an arm64 VPS, or to try a change before pushing
it, `UWUMAIL_GATEWAY_BUILD=local` builds it with Docker on your machine. The
script then copies it over and runs
[`deploy/gateway/install.sh`](../deploy/gateway/install.sh).

Either way, `install.sh`

- creates the system user `uwumail-gateway`,
- installs the program to `/usr/local/bin/uwumail-gateway`,
- puts the configuration to `/etc/uwumail-gateway/gateway.toml` (an existing one
  is kept), see [`deploy/gateway/gateway.toml`](../deploy/gateway/gateway.toml),
- installs and starts the systemd service, which runs as that user with nothing
  but the permission to use the low ports,
- shows the pairing code.

The same command updates an installed gateway. The key and the pairing live in
`/var/lib/uwumail-gateway`.

On the VPS:

| Command | |
| --- | --- |
| `sudo uwumail-gateway code` | Shows the pairing code (also in `journalctl -u uwumail-gateway`) |
| `sudo uwumail-gateway unpair` | Forgets the paired server; the gateway disconnects it and makes a new code |
| `uwumail-gateway --config /etc/uwumail-gateway/gateway.toml check-config` | Checks the configuration |

From 0.1.2 the commands read `/etc/uwumail-gateway/gateway.toml` by themselves
when it exists, like the service does, and `--config` names another file. A
gateway before that only reads it with
`--config /etc/uwumail-gateway/gateway.toml`: `check-config` without it checks
the defaults and not your file, and once you set `public_addresses`, `tunnel`
or `state_dir` there, `code` and `unpair` need it too, or the code carries the
wrong addresses or port.

A firewall that only lets through what the gateway needs, for example with
nftables:

```
table inet filter {
  chain input {
    type filter hook input priority 0; policy drop;
    ct state established,related accept
    iif lo accept
    meta l4proto { icmp, ipv6-icmp } accept
    tcp dport { 22, 25, 80, 443, 465, 587, 993 } accept
    udp dport 443 accept
    # Do not leave these out on a VPS that gets its addresses by DHCP. A DHCPv6
    # answer arrives from another address than the multicast one it was asked
    # for, so connection tracking cannot pair it with the request: without this
    # rule the IPv6 address quietly expires a few hours later and the machine
    # loses IPv6 altogether. (It cost me exactly that.)
    udp dport { 68, 546 } accept
  }
}
```

## Pair your server

A pairing code looks like `uwugw1…` and contains the gateway's addresses, the
fingerprint of its certificate and a one-time token.

**In the configuration:** the way for a fresh install, before the first start.
The host name already points to the gateway, so the setup assistant can only be
reached through it once the server is paired; an unpaired gateway closes port
443 and answers 503 on port 80. With the stock `compose.yaml`, the code goes
into `.env`:

```bash
UWUMAIL_GATEWAY_CODE=uwugw1…
```

`compose.yaml` hands it to the server as `UWUMAIL_GATEWAY__CODE`. With your own
compose file, set that variable yourself; in a TOML configuration it is

```toml
[gateway]
code = "uwugw1…"
```

The stock `compose.yaml` always sets `UWUMAIL_GATEWAY__CODE`, empty when `.env`
has no code, and the environment wins over a config file. With that file the
code belongs in `.env`; a `[gateway] code` in a mounted TOML file is ignored.

Then start the server. When it already runs, `sudo docker compose up -d` picks
up the changed `.env`; `restart` does not. The server pairs without an admin
account. The log shows `paired with the UwUMail Gateway` and then
`connected to the UwUMail Gateway`, and as soon as the tunnel is connected the
server asks Let's Encrypt for its certificate (`got a fresh certificate`).

**From the command line** (from 0.1.2), for when you cannot reach the portal
and do not want to put the code into the configuration:

```bash
sudo docker compose exec uwumail uwumail-server gateway pair uwugw1…
sudo docker compose restart uwumail
```

It refuses a code that differs from `gateway.code` in the configuration,
because that one wins on every start. After the restart,
`uwumail-server gateway show` tells whether the gateway accepted the code.

**In the portal:** another way, for a portal you can reach without the
gateway: a server whose name still points to it, or the server's address in
your network. The setup assistant has a step *Reachability* right after
the admin account. It checks whether your connection is a home connection
(Spamhaus PBL), whether its reverse DNS was made up by the provider, which
network it belongs to and whether port 25 works, and recommends a gateway or
sending directly. Choose *Through a gateway*, paste the code and pair; the
panel shows when the tunnel is up and where the host name has to point. After a
pairing from the configuration it shows the gateway as already paired. The
same checks and the pairing stay under *Server → Setup*. Pairing and
forgetting ask for your password again.

The token only works once: the gateway now knows your server by its
certificate, and the server keeps its key and the pairing in its database, so
the code may stay in the configuration. A code there that differs from a
pairing made in the portal replaces it at the next restart, so keep the two the
same or take the code out. `uwumail-server gateway show` shows the pairing.

From then on, mail to other servers **only** leaves through the gateway, also
while it is unreachable (it waits in the queue instead of leaking your home
address). Servers in your own network, like fixed routes to private
addresses, are still reached directly. The health overview has a *Gateway* area
that turns yellow when the tunnel is down and red after five minutes.

`uwumail-server gateway forget`, or *Forget gateway* in the portal, removes the
pairing together with this server's tunnel key, and a new data volume has
neither. The server then pairs with a new key, and the gateway, which still
knows the old one, refuses it: run `sudo uwumail-gateway unpair` on the VPS and
use the new code. The same goes for pairing another server with the gateway.

## DNS records

Everything that pointed to your server now points to the gateway:

| Record | Value |
| --- | --- |
| `mail.example.com A` / `AAAA` | the gateway's addresses (shown in its log and in the pairing) |
| `example.com MX` | `10 mail.example.com.`, as before |
| `example.com TXT` | `v=spf1 mx -all` covers the gateway, because the MX host points to it |
| Reverse DNS of the gateway's addresses | `mail.example.com`, set at the VPS provider |

In your home network, a local DNS entry for `mail.example.com` can keep pointing
straight to your server, so mail apps at home don't take the detour. The entry
moves the whole name, so the home machine then has to answer on ports 443, 993,
465 and 587 itself. When another web server holds port 443 there, the simplest
is to skip the entry and use the gateway from home too; see
[deployment.md](deployment.md#behind-a-reverse-proxy).

## Good to know

- **Only one server per gateway.**
- **Not everything goes through the gateway.** DNS lookups (SPF, DKIM, MX),
  certificate renewals and MTA-STS policies are fetched from home. None of that
  ends up in DNS records or mail headers.
- **Trust the VPS like the server.** Someone who breaks into it cannot read TLS
  connections, but could strip STARTTLS on port 25 like any network on the way
  (MTA-STS protects against that), and could hand connections to your server
  with made-up client addresses. Keep it updated and locked down.
- **The tunnel waits a moment.** A new home address after a reconnect means a
  new tunnel after a few seconds; mail senders in that moment get a 421 and try
  again.
