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

Log in with the full address and password. IMAP and JMAP follow in the next
steps (see the roadmap).

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

## Updates

```bash
docker compose pull && docker compose up -d
```

Database migrations run automatically on start.
