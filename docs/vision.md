# Vision

**UwUMail Server is the mail server everyone can run.** A family, a club or a
small team gets their own domain, their own mailboxes and the privacy of their
own hardware, without becoming a mail administrator.

## Principles

1. **Self-hosted for everyone.** One container, a setup assistant and plain
   explanations. When something is wrong (DNS, port 25, a blocklist), the
   server says what and how to fix it.
2. **Simple first, powerful on request.** The admin panel has a Simple mode
   (people, domains, a traffic light) and a Pro mode (queue, logs, DKIM,
   spam scores, raw settings), like the Simple and Pro layouts of the app.
3. **Modern protocols, no lock-in.** JMAP is the primary protocol and powers
   the UwUMail apps and web mail. IMAP, SMTP submission, CalDAV, CardDAV and
   ManageSieve keep every other app working.
4. **Home is a valid place for a server.** An optional UwUMail Gateway on a
   small VPS provides a fixed IP and port 25 and tunnels everything home,
   buffering mail while the home connection is down.
5. **Private by default.** No telemetry. Submitted mail does not reveal the
   sender's IP address or device name. Update checks can be turned off.
6. **Cute, not childish.** The interface and mail to our own people are
   playful by default (one switch makes them neutral). Mail to strangers is
   neutral by default, with an optional light, friendly tone.
7. **Open.** AGPL-3.0: anyone may run, study and change it; whoever offers a
   modified version as a service shares the changes.

## Who we build for

| Person | What they need |
| --- | --- |
| "I want my own domain for the family" | Guided setup, accounts in a few clicks, mail that arrives |
| "Our club needs info@ and shared mailboxes" | Groups, shared folders, aliases, a web mail everyone can use |
| "My server lives at home" | Gateway for port 25 and a fixed IP, buffering, good deliverability |
| "I run infrastructure anyway" | Reverse proxy mode, OIDC/LDAP login, Prometheus metrics, backups to S3 |

## Size

Built for 1 to about 500 mailboxes on one machine, SQLite plus files, so a
Raspberry Pi is enough. Multi-server clusters and hosting providers with
tenants are not a goal.
