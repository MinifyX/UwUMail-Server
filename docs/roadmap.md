# Roadmap

Each step ships as its own commits, container image and test deployment.

## 1. Mail core ✅ in progress

- [x] Storage: SQLite (WAL) + content-addressed blob files, migrations
- [x] Domains, accounts (argon2id), aliases, sub-addresses, catch-all, postmaster/abuse routing
- [x] Mailboxes with IMAP UIDs, threading, full-text index, change log (JMAP states / CONDSTORE), quotas
- [x] SMTP receiving (MX): STARTTLS, recipient checks, relay protection, pipelining, CHUNKING, size limits
- [x] SPF, DKIM and DMARC verification, Authentication-Results, DMARC reject/quarantine
- [x] Submission on 587 (STARTTLS) and 465 (TLS) with AUTH, login throttling, sender ownership
- [x] Outbound queue with MX lookup, opportunistic TLS, relay mode, retries and leases
- [x] DKIM keys per domain (RSA + Ed25519), dual signing
- [x] Bounces (RFC 3464) in German and English with internal/external tone
- [x] TLS: Let's Encrypt (HTTP-01), certificate files with reload, self-signed for testing
- [x] Command line: domains, DNS records, accounts, aliases, queue
- [x] Container image (amd64 + arm64), compose file, local two-server test stack
- [ ] Public test instance

## 2. JMAP ✅ in progress

- [x] Session resource at `/.well-known/jmap`, Basic authentication with login throttling, result references, creation ids
- [x] Uploads and downloads (whole messages and single attachments), access limited to the owning account
- [x] Mail: `Mailbox/get|changes|query|set`, `Email/get|changes|query|set|import|parse`, `Thread/get|changes`, `SearchSnippet/get`
- [x] Submission: `Identity/get|changes|set`, `EmailSubmission/get|changes|query|set` with `onSuccessUpdateEmail` / `onSuccessDestroyEmail`
- [x] Push over EventSource
- [x] `VacationResponse/get|set`
- [x] Vacation auto-replies when mail arrives (once per sender per week, never to lists or machines)
- [ ] App passwords and Bearer tokens
- [ ] WebSocket push, delayed sending (undo window), `Email/copy`, query changes
- [x] The UwUMail app's JMAP integration test passes against this server (`dev/client-compat.sh`)
- [ ] Tested with other JMAP clients

## 3. Setup assistant and admin panel ✅ in progress

- [x] Web portal: JSON API with session cookies and CSRF protection, React app embedded in the binary, one login for everyone
- [x] Portal shell with Nyu, German and English, playful/neutral tone, Simple and Pro mode, light/dark theme, phone layout
- [x] My account overview (addresses, storage, settings for mail apps) and a first server overview for admins
- [x] People: invite with a one-time link, change role and storage limit, lock out (mail keeps arriving), 30-day trash, aliases
- [x] Change log of admin changes (portal and command line)
- [x] Domains: DNS check of MX, SPF, DMARC and DKIM from the root servers down, catch-all, DKIM key rotation
- [x] Queue (senders, recipients, errors, never subjects) with retry and delete; live server log in Pro mode
- [ ] Web setup with a one-time code: domain, admin, certificate, DNS check, port 25 / rDNS / blocklist check
- [ ] Admin panel, Simple and Pro mode, German and English, playful/neutral tone, Nyu
- [ ] Self-service: password, 2FA, app passwords, forwarding, vacation, aliases, storage
- [ ] Autoconfig, Autodiscover, Apple configuration profiles, MTA-STS, TLS-RPT
- [ ] Update notices with changelog

## 4. Spam filter

- [ ] Rspamd rules ported to Rust: header/body patterns, URL and phishing checks, MIME tricks
- [ ] DNS blocklists, greylisting, sender reputation
- [ ] Bayes classifier that learns from "Spam" / "Not spam" in the apps
- [ ] Optional external Rspamd, optional ClamAV container

## 5. IMAP

- [ ] IMAP4rev2 with CONDSTORE/QRESYNC, IDLE, MOVE, SPECIAL-USE, QUOTA, ACL

## 6. Web mail and external mailboxes

- [ ] UwUMail web client (built from the app repository) served at `/`, switchable in the admin panel
- [ ] External mailboxes (Gmail, GMX, ...) through the UwUMail engine, shown as extra JMAP accounts

## Later

- [ ] Sieve filters and ManageSieve
- [ ] CalDAV, CardDAV, JMAP Calendars and Contacts
- [ ] UwUMail Gateway: WireGuard tunnel from a VPS, PROXY protocol, buffering, outbound via fixed IP
- [ ] OAuth 2 / OpenID Connect provider for mail apps; login via external OIDC or LDAP
- [ ] Passkeys
- [ ] Migration assistant (IMAP import from the old provider)
- [ ] Groups, shared mailboxes, masked addresses
- [ ] Settings sync for the UwUMail apps, send later and snooze on the server
- [ ] Web Push / UnifiedPush, sender pictures from the server
- [ ] Scheduled backups (folder, S3, SFTP) with restore per mailbox
- [ ] Admin alerts, statistics, Prometheus metrics, DMARC and TLS report analysis
