<p align="center">
  <img src="brand/uwumail-app-icon.svg" width="112" alt="UwUMail logo" />
</p>

<h1 align="center">UwUMail Server</h1>

<p align="center">
  Your own cute mail server, for everyone. (=^･ω･^=)<br/>
  JMAP · SMTP · IMAP · CalDAV/CardDAV · one Docker container
</p>

---

UwUMail Server is a self-hosted mail server written in Rust. It is the home
base for the [UwUMail](https://github.com/MinifyX/UwUMail-Client) apps and
works with every other mail app too. It is built so that a family, a club or a
small team can run their own mail without being a mail admin:

- **One container.** Mail server, spam filter, admin panel and web mail in a
  single image for amd64 and arm64 (yes, a Raspberry Pi is enough).
- **Guided setup.** A setup assistant walks you through domain, certificate
  and DNS records and checks everything live.
- **Delivers from home.** Blocked port 25 or no fixed IP? The optional
  UwUMail Gateway on a small VPS tunnels mail to your server at home.
- **Modern protocols.** JMAP first, plus IMAP, SMTP submission, CalDAV,
  CardDAV and Sieve filters.
- **Simple or Pro.** The admin panel has the same two modes as the app: a
  calm overview for everyone, every detail for those who want it.
- **Private by default.** No telemetry. Your mail stays on your hardware.

> **Status:** early development, not usable for real mail yet. See the
> [roadmap](docs/roadmap.md).

## Project layout

| Path | What lives there |
| --- | --- |
| `crates/uwumail-server` | The server binary: configuration, listeners, TLS, HTTP, command line |
| `crates/uwumail-smtp` | SMTP receiving and submission, outbound queue, DKIM, SPF/DMARC checks |
| `crates/uwumail-store` | SQLite + file storage: domains, accounts, mailboxes, messages, queue |
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
[docs/deployment.md](docs/deployment.md) and [docs/configuration.md](docs/configuration.md).

## License

UwUMail Server is licensed under the [GNU Affero General Public License v3.0](LICENSE).
If you run a modified version as a service, you have to publish your changes.
