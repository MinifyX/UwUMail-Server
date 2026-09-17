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

From a clone of this repository, with the GitHub CLI (`gh`) and SSH access to
the VPS:

```bash
UWUMAIL_GATEWAY_HOST=root@203.0.113.10 scripts/deploy-gateway.sh
```

It takes the gateway that CI built from the newest successful commit on `main`
and checks its SHA-256 sum (`UWUMAIL_GATEWAY_RUN=<run id>` picks another run).
CI builds for amd64 only; for an arm64 VPS, or to try a change before pushing
it, `UWUMAIL_GATEWAY_BUILD=local` builds it with Docker on your machine. The
script then copies it over and runs
[`deploy/gateway/install.sh`](../deploy/gateway/install.sh), which

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
| `uwumail-gateway code` | Shows the pairing code (also in `journalctl -u uwumail-gateway`) |
| `uwumail-gateway unpair` | Forgets the paired server; the gateway disconnects it and makes a new code |
| `uwumail-gateway check-config` | Checks the configuration |

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

**In the portal:** the setup assistant has a step *Reachability* right after
the admin account. It checks whether your connection is a home connection
(Spamhaus PBL), whether its reverse DNS was made up by the provider, which
network it belongs to and whether port 25 works, and recommends a gateway or
sending directly. Choose *Through a gateway*, paste the code and pair; the
panel shows when the tunnel is up and where the host name has to point. The
same checks and the pairing stay under *Server → Setup*. Pairing and
forgetting ask for your password again.

**In the configuration:** put the code into the configuration,

```toml
[gateway]
code = "uwugw1…"
```

or into `UWUMAIL_GATEWAY__CODE`, and restart the server. The log shows
`paired with the UwUMail Gateway` and then `connected to the UwUMail Gateway`.

The token only works once: the gateway now knows your server by its
certificate, and the server keeps its key and the pairing in its database, so
the code may stay in the configuration. `uwumail-server gateway show` shows the
pairing, `uwumail-server gateway forget` removes it.

From then on, mail to other servers **only** leaves through the gateway, also
while it is unreachable (it waits in the queue instead of leaking your home
address). Servers in your own network, like fixed routes to private
addresses, are still reached directly. The health overview has a *Gateway* area
that turns yellow when the tunnel is down and red after five minutes.

To pair another server with the gateway, run `uwumail-gateway unpair` on the
VPS and use the new code.

## DNS records

Everything that pointed to your server now points to the gateway:

| Record | Value |
| --- | --- |
| `mail.example.com A` / `AAAA` | the gateway's addresses (shown in its log and in the pairing) |
| `example.com MX` | `10 mail.example.com.`, as before |
| `example.com TXT` | `v=spf1 mx -all` covers the gateway, because the MX host points to it |
| Reverse DNS of the gateway's addresses | `mail.example.com`, set at the VPS provider |

In your home network, a local DNS entry for `mail.example.com` can keep pointing
straight to your server, so mail apps at home don't take the detour.

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
