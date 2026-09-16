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

- **One container.** Mail server and admin panel in a single image for amd64
  and arm64 (yes, a Raspberry Pi is enough); spam filter and web mail are
  meant to join them.
- **Guided setup (planned).** A setup assistant that walks you through domain,
  certificate and DNS records and checks everything live.
- **Delivers from home (planned).** Blocked port 25 or no fixed IP? An
  optional UwUMail Gateway on a small VPS tunnels mail to your server at home.
- **Modern protocols.** JMAP first and SMTP submission today; IMAP, CalDAV,
  CardDAV and Sieve filters are planned.
- **Simple or Pro.** The admin panel has the same two modes as the app: a
  calm overview for everyone, every detail for those who want it.
- **Private by default.** No telemetry. Your mail stays on your hardware.

> **Status:** early development. I run a test instance for my own domain, but
> it isn't ready for anyone's real mail yet. The [roadmap](docs/roadmap.md)
> shows what's done and what I'd like to do next.

## Project layout

| Path | What lives there |
| --- | --- |
| `crates/uwumail-server` | The server binary: configuration, listeners, TLS, HTTP, command line |
| `crates/uwumail-smtp` | SMTP receiving and submission, outbound queue, DKIM, SPF/DMARC checks |
| `crates/uwumail-jmap` | JMAP: mail, submission, uploads and downloads, push |
| `crates/uwumail-web` | The web portal: JSON API and the embedded admin and account app |
| `crates/uwumail-store` | SQLite + file storage: domains, accounts, mailboxes, messages, queue |
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
[docs/deployment.md](docs/deployment.md), [docs/configuration.md](docs/configuration.md) and
[docs/spam-filter.md](docs/spam-filter.md).

## License

UwUMail Server is licensed under the [GNU Affero General Public License v3.0](LICENSE):
use it, change it, fork it. If you run a modified version as a service, you
have to publish your changes.
