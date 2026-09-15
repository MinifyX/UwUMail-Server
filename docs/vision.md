# Vision

**UwUMail Server is the mail server I want to run.** Every self-hosted mail
server I tried annoyed me in one way or another, so I started building my own.
It isn't trying to be the mail server for everyone. If it happens to fit you
too, great.

I still want it to be good enough that a family, a club or a small team could
get their own domain, their own mailboxes and the privacy of their own
hardware, without becoming mail administrators.

## What to expect

- **A hobby project.** No company, no team, no schedule, no support. I build
  what I need and what I find fun, when I have time and feel like it. That can
  mean a lot of changes in a week and then nothing for months.
- **Written with AI.** I decide what the server should do and how it should
  work; almost all of the code is written with Claude, because I'm honestly not
  a great programmer. Automated tests catch what they can. If that's a
  dealbreaker, that's completely fine, there are other mail servers.
- **Free to take.** Anyone may run it, fork it and turn it into something else
  under the AGPL-3.0.

## Principles

These are the rules I build by.

1. **Self-hosting without the pain.** One container, a setup assistant and
   plain explanations. When something is wrong (DNS, port 25, a blocklist),
   the server says what and how to fix it.
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
6. **Cute, not childish.** The interface and mail to the server's own people
   are playful by default (one switch makes them neutral). Mail to strangers
   is neutral by default, with an optional light, friendly tone.
7. **Open.** AGPL-3.0: anyone may run, study and change it; whoever offers a
   modified version as a service shares the changes.

## Who it's for

Me, first of all. These are the setups I had in mind; if you recognize
yourself, UwUMail Server might suit you too.

| Person | What they need |
| --- | --- |
| "I want my own domain for the family" | Guided setup, accounts in a few clicks, mail that arrives |
| "Our club needs info@ and shared mailboxes" | Groups, shared folders, aliases, a web mail everyone can use |
| "My server lives at home" | Gateway for port 25 and a fixed IP, buffering, good deliverability |
| "I run infrastructure anyway" | Reverse proxy mode, OIDC/LDAP login, Prometheus metrics, backups to S3 |

Much of this is still on the [roadmap](roadmap.md).

## Size

Built for 1 to about 500 mailboxes on one machine, SQLite plus files, so a
Raspberry Pi is enough. Multi-server clusters and hosting providers with
tenants are not a goal.
