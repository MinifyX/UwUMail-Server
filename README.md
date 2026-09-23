<p align="center">
  <img src="brand/uwumail-app-icon.svg" width="112" alt="UwUMail logo" />
</p>

<h1 align="center">UwUMail Server</h1>

<p align="center">
  The mail server I build for myself, because every self-hosted one annoyed me. (=^･ω･^=)<br/>
  JMAP · SMTP · IMAP · CalDAV/CardDAV · one Docker container
</p>

---

## Why this exists

I'm building UwUMail Server for myself. Every self-hosted mail server I tried
annoyed me in one way or another, and so did the mail clients, so I started
building my own, the way I want it. The app half lives in
[UwUMail](https://github.com/MinifyX/UwUMail-Client).

- **Just for fun.** No company, no team, no schedule, no promises. I work on it
  when I have time and feel like it, so don't expect steady development, and
  don't be surprised by long breaks.
- **Written with AI.** Almost all of the code is written with Claude, because
  I'm honestly not a great programmer. Not your thing? No hard feelings, just
  pick something else.
- **Use it, fork it, do what you want with it.** The license only asks one
  thing: changed versions stay open, even when you only run them as a service.
- **No support.** Issues and pull requests are okay, but I might answer late or
  not at all, and I mostly build what I need myself.

## What it is

UwUMail Server is a self-hosted mail server written in Rust. It is the home
base for the UwUMail apps and works with other mail apps too. I want it to be
something a family, a club or a small team can run without being a mail admin:

- **One container.** Mail server, spam filter and admin panel in a single
  image for amd64 and arm64 (yes, a Raspberry Pi is enough); web mail is meant
  to join them.
- **Guided setup.** A setup assistant walks you through admin account, domain,
  DNS records and sending, checks everything live and ends with a test mail.
- **Delivers from home.** Blocked port 25 or no fixed IP? An optional
  [UwUMail Gateway](docs/gateway.md) on a small VPS tunnels mail and web to
  your server at home and sends from its own address.
- **Mail, calendars, contacts.** JMAP, IMAP and SMTP for mail apps, CalDAV and
  CardDAV for calendars and contacts, and the same calendars and address books
  as JMAP Calendars and JMAP Contacts for the webmail and the apps; [Sieve mail rules](docs/sieve.md) over JMAP and
  ManageSieve.
- **Spam filter and, if you want, a virus scanner.** The filter learns, keeps
  sender and word lists and fetches known-bad lists by itself; an optional
  [ClamAV beside the server](docs/antivirus.md) turns infected mail away before
  it is taken.
- **Backups and updates built in.** Nightly deduplicated, encrypted backups to
  SFTP, and the portal tells you when a new version is out.
- **Everything in one panel.** Accounts, domains, queue, logs, spam and
  settings, in German or English and in a playful or a plain tone.
- **Private by default.** No telemetry. Your mail stays on your hardware.

> **Status:** early, but I run my own mail on it. Set up backups, and remember
> there's no support. The [roadmap](docs/roadmap.md) shows what's done and what
> I'd like to do next.

## Install

You need a domain and a Linux machine with Docker: either a server with a public
address, or a machine at home plus a small VPS for the
[UwUMail Gateway](docs/gateway.md).

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo bash install.sh
```

It asks for the host name and a few other things, sets up `/opt/uwumail`,
starts the server and shows a one-time code. Then open
`https://mail.example.com/setup` and enter it. Every answer is a flag as well,
so it runs without questions too:

```bash
sudo bash install.sh --hostname mail.example.com --email me@example.org --yes
```

The next version, later on:

```bash
cd /opt/uwumail && sudo bash update.sh
```

The whole way with DNS, the gateway, mail apps and backups is in
**[docs/install.md](docs/install.md)**.

Coming from mailcow? [docs/migrating-from-mailcow.md](docs/migrating-from-mailcow.md).

## Project layout

| Path | What lives there |
| --- | --- |
| `crates/uwumail-server` | The server binary: configuration, listeners, TLS, HTTP, command line |
| `crates/uwumail-smtp` | SMTP receiving and submission, outbound queue, DKIM, SPF/DMARC checks |
| `crates/uwumail-jmap` | JMAP: mail, submission, calendars, contacts, uploads and downloads, push |
| `crates/uwumail-imap` | IMAP for mail apps |
| `crates/uwumail-dav` | CalDAV and CardDAV |
| `crates/uwumail-backup` | Deduplicated, encrypted backups over SFTP |
| `crates/uwumail-web` | The web portal: JSON API and the embedded admin and account app |
| `crates/uwumail-store` | SQLite + file storage: domains, accounts, mailboxes, messages, queue |
| `crates/uwumail-tunnel` | The QUIC tunnel between a server and its UwUMail Gateway |
| `crates/uwumail-gateway` | The UwUMail Gateway program for a VPS |
| `deploy/gateway` | systemd service, configuration and install script for the gateway |
| `install.sh`, `update.sh` | Setting the server up on a machine, and bringing it to the next version |
| `web/` | The portal's React app |
| `docker/` | Container images |
| `docs/` | Vision, architecture, configuration and deployment guides |

## Development

You need Rust (stable), Docker and Node.js (for the smoke test).

```bash
cargo test --workspace
docker compose -f dev/compose.yaml up -d --build   # two servers, a.test and b.test
bash dev/seed.sh && node dev/smoke.mjs
```

More in [docs/development.md](docs/development.md). Running it for real:
[docs/install.md](docs/install.md), [docs/deployment.md](docs/deployment.md),
[docs/configuration.md](docs/configuration.md), [docs/spam-filter.md](docs/spam-filter.md)
and [docs/antivirus.md](docs/antivirus.md).

## License

UwUMail Server is licensed under the [GNU Affero General Public License v3.0](LICENSE):
use it, change it, fork it. If you run a modified version as a service, you
have to publish your changes.
