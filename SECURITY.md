# Security

UwUMail Server handles people's mail, so security reports are very welcome.

Please do **not** open a public issue for vulnerabilities. Report them
privately through GitHub's "Report a vulnerability" (Security tab) or to me
([@MinifyX](https://github.com/MinifyX)) directly.

The server runs real mail in production and is actively developed, almost
entirely with AI (Claude). It's one person, so there are no guaranteed response
times, but security reports come before everything else: they get fixed and
released as soon as I can. Please give me the chance to ship a fix before
publishing details.

## Supported versions

Only the newest release gets fixes, security fixes included. `update.sh` brings
an installation up to date and rolls back when an update doesn't come up
healthy.

## Audit

Security sweeps are done with Claude, and written down in full:

- [docs/security-audit.md](docs/security-audit.md) — before the code and the
  container image went public.
- [docs/security-audit-2026-09.md](docs/security-audit-2026-09.md) — the server
  again, and the gateway and its tunnel for the first time.
- [docs/security-audit-2026-09-18.md](docs/security-audit-2026-09-18.md) — the
  whole stack, including the desktop client, and everything built since.
- [docs/security-audit-0.16.0.md](docs/security-audit-0.16.0.md) — server and
  webmail once more, everything up to 0.16.0; every finding from Medium up fixed.
- [docs/security-audit-0.17.0.md](docs/security-audit-0.17.0.md) — what 0.17.0
  added: pictures, Libravatar and the Face header, one-click unsubscribe, and
  the new parts of JMAP Calendars; every finding from Medium up fixed.
- [docs/security-audit-0.3.0.md](docs/security-audit-0.3.0.md) — what 0.3.0
  added: the virus scanner and the DNS records at Cloudflare.
- [docs/security-audit-0.4.0.md](docs/security-audit-0.4.0.md) — what 0.4.0
  added, and every way into the server once more: service accounts with a
  switch per protocol, the two shell scripts, and the helpers beside the server.

Each one lists the findings, what was checked, what was hardened and the limits
that are known and accepted. They are honest reviews, not independent
certifications.
