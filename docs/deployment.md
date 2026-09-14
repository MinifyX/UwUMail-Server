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

At home without a fixed IP or with a blocked port 25? The UwUMail Gateway
(see the roadmap) will solve that; until then use `[delivery.relay]`.

## Start

```bash
mkdir uwumail && cd uwumail
curl -O https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/compose.yaml
curl -o .env https://raw.githubusercontent.com/MinifyX/UwUMail-Server/main/.env.example
# edit .env: UWUMAIL_HOSTNAME=mail.example.com
docker compose up -d
docker compose logs -f
```

The server gets a Let's Encrypt certificate as soon as `mail.example.com`
points to it and port 80 is reachable. Until then it uses a self-signed one.

## Domains and accounts

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
| `_dmarc.example.com TXT "v=DMARC1; p=quarantine; …"` | What receivers do with forged mail |
| `mail.example.com A/AAAA` and PTR | The server itself |

Show them again any time with `uwumail-server domain dns example.com`.

## Mail apps

| | Server | Port | Security |
| --- | --- | --- | --- |
| Sending | `mail.example.com` | 465 | TLS |
| Sending | `mail.example.com` | 587 | STARTTLS |

Log in with the full address and password. JMAP apps (like UwUMail) only need
`https://mail.example.com`; they find everything else at `/.well-known/jmap`.
IMAP follows in a later step (see the roadmap).

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

## Next to an existing mail server

You can try UwUMail on a (sub)domain while an existing mail server such as
Mailcow keeps port 25 and all other domains. Ready-made files:
[`deploy/next-to-mailserver`](../deploy/next-to-mailserver).

1. **Existing mail server:** add the test domain as a relay domain (Mailcow:
   *Domains → Add domain → Relay this domain, relay all recipients*) and a
   transport map `uwu.example.com → [192.0.2.30]:25` (Mailcow: *Routing →
   Transport maps*).
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
   DMARC record (start with `p=none`).

Mail apps in your own network connect straight to the UwUMail machine on 465
or 587; a local DNS entry for the host name keeps certificates valid.

## Updates

```bash
docker compose pull && docker compose up -d
```

Database migrations run automatically on start.
