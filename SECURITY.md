# Security

UwUMail Server handles people's mail, so security reports are very welcome.

Please do **not** open a public issue for vulnerabilities. Report them
privately through GitHub's "Report a vulnerability" (Security tab) or to me
([@MinifyX](https://github.com/MinifyX)) directly.

This is a hobby project that I build in my spare time, almost entirely with AI
(Claude), so I can't promise response times. Security reports still come first
when I get to them; please give me the chance to ship a fix before publishing
details.

Until the first release the project is in early development and not meant for
production mail.

## Audit

Security sweeps are done with Claude, and written down in full:

- [docs/security-audit.md](docs/security-audit.md) — before the code and the
  container image went public.
- [docs/security-audit-2026-09.md](docs/security-audit-2026-09.md) — the server
  again, and the gateway and its tunnel for the first time.
- [docs/security-audit-2026-09-18.md](docs/security-audit-2026-09-18.md) — the
  whole stack, including the desktop client, and everything built since.

Each one lists the findings, what was checked, what was hardened and the limits
that are known and accepted. They are honest reviews, not independent
certifications.
