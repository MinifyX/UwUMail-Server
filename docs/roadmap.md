# Roadmap

This is my to-do and wish list, not a promise. There are no dates: things get
built when I need them or feel like it, the order changes, and some of it may
never happen. See [Why this exists](../README.md#why-this-exists).

Each step ships as its own commits, container image and test deployment.

## 1. Mail core ✅ in progress

- [x] Storage: SQLite (WAL) + content-addressed blob files, migrations
- [x] Domains, accounts (argon2id), aliases, sub-addresses, catch-all, postmaster/abuse routing
- [x] Forwarding addresses without a mailbox (domain page, `uwumail-server forward`), no spam passed on
- [x] Sending as any address of a domain for chosen people (person page, `uwumail-server account send-as`)
- [x] Import from mailcow (`scripts/mailcow-export.sh`, `uwumail-server import mailcow`): people with their password hashes, aliases, forwarding, app passwords, sender lists, spam limits, DKIM keys, calendars and contacts
- [x] Copying mail over IMAP with a dovecot master user, repeatable for what arrived since (`uwumail-server import imap`)
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
- [x] Portal shell with Nyu, German and English, playful/neutral tone, light/dark theme, phone layout
- [x] My account overview (addresses, storage, settings for mail apps) and a first server overview for admins
- [x] Accounts: invite with a one-time link, change role and storage limit, lock out (mail keeps arriving), 30-day trash, aliases
- [x] Service accounts for programs: no portal login, app passwords an admin makes, a switch per protocol, and no mailbox at all when IMAP and JMAP are off (mail refused or handed to one address)
- [x] Change log of admin changes (portal and command line), every entry opening to who, what, when, from where and the details as they are stored
- [x] Domains: DNS check of MX, SPF, DMARC and DKIM from the root servers down, catch-all, DKIM key rotation
- [x] Queue (senders, recipients, errors, never subjects) with retry and delete; live server log
- [x] Server settings in the admin panel, applied without a restart; config file and environment take precedence
- [x] Health overview: DNS, certificate, outgoing mail (relay/port 25 probe, stuck mail, bounces), disk and mailbox space
- [x] Setup assistant with a one-time code from the log: first domain and admin, DNS records (optionally added at Cloudflare), sending route and port 25 both ways, reverse DNS, blocklists on request, test mail with a reply from outside; the checks stay under Server → Setup
- [x] Admin panel, German and English, playful/neutral tone, Nyu
- [x] HSTS on the server's own HTTPS once it has a real certificate
- [x] Security in My account: password change, authenticator app with recovery codes, passkeys, app passwords with scopes and expiry, browser sessions, activity list and notice mails
- [x] Forwarding in My account (people on this server at once, other servers after a confirmation link, SRS, admin locks) and away messages
- [x] Own addresses (domains opened by an admin, limit per person, reserved for 30 days after deleting) and storage per folder with emptying Trash and Junk
- [x] MTA-STS per domain (testing, then enforce with a suggestion after 14 clean days), policy served on `mta-sts.<domain>` with its certificate, and followed when delivering to other domains
- [x] DMARC and TLS reports read by the server and shown per domain, with suggestions for a stricter DMARC policy
- [x] Recommended records in the DNS check: TLS reporting, SRV for JMAP and submission
- [x] Update notices with the changelog; the update itself is `update.sh` on the machine, with a backup, a health check and a way back
- [x] `install.sh`: host name, gateway code, virus scanner and host helper in one go, from an empty machine to the one-time code

## 4. Spam filter ✅ in progress

- [x] Score for mail from other servers with the rules that fired, `X-Spam-Score` / `X-Spam-Status` headers, score in the server log
- [x] DNS blocklists (Spamhaus ZEN, SpamCop, Barracuda), greylisting only for suspicious senders, sender reputation by DMARC domain or network
- [x] Thresholds for greylisting, Junk and refusing (off by default) in the admin panel
- [x] "Spam" / "Not spam" in the apps (moving to or from Junk, `$junk` / `$notjunk`) corrects the sender's reputation
- [x] Content rules after Rspamd's example: phishing links and look-alike domains, faked display names, dangerous attachments (also inside zip archives), header and MIME oddities, Spamhaus DBL for link domains
- [x] Word lists (words, phrases, Rspamd-style expressions) per person, domain and server, pasted or subscribed to by link
- [x] Built-in lists: malware links and attachments (abuse.ch), spam subjects (mailcow), throwaway and freemail domains and link shorteners (Rspamd)
- [ ] More content rules: SURBL/URIBL
- [x] Bayes filter that learns from "Spam" / "Not spam", clear cases and once from sorted mail, with knowledge for the whole server and per person, tokens only as keyed hashes
- [x] Allowed and blocked senders (IP address or network, confirmed host name, address, domain) per person, domain and server
- [x] "Block" in the apps puts the sender on the person's list on the server (JMAP `SenderList`, see jmap-senders.md)
- [x] Optional ClamAV beside the server: infected mail is turned away at the door, its own page in the portal, off until it is started (docs/antivirus.md)
- [ ] Optional external Rspamd

## 5. IMAP

- [x] IMAP4rev1 on port 993 (TLS) with IDLE, UIDPLUS, MOVE, SPECIAL-USE, LIST-EXTENDED, LIST-STATUS, ESEARCH, CONDSTORE, QRESYNC, QUOTA, UTF8=ACCEPT, also through the gateway
- [ ] IMAP4rev2, ACL and shared folders
- [x] Autoconfig, Autodiscover and Apple configuration profiles with their own app password
- [x] CalDAV and CardDAV with sync-collection, calendar-query and multiget, also in the Apple profile
- [ ] Signed Apple configuration profiles, scheduling (iTIP), shared calendars

## 6. Web mail and external mailboxes ✅ in progress

- [x] UwUMail webmail (its own repository, built from the app's interface) served at `/mail`, switchable for the server and per account (docs/webmail.md)
- [ ] Delayed sending, signatures, sender pictures and address suggestions on the server, so the webmail stops doing without them
- [x] Fetched mailboxes: mail from another provider's IMAP mailbox, emptied into someone's own and judged here like any other, with rules of its own for what a fetched message can still be asked (docs/fetch.md)
- [ ] Sending as a fetched address, over the provider's own outgoing server
- [ ] External mailboxes shown as extra JMAP accounts, with their folders and with changes going back

## UwUMail Gateway ✅ in progress

- [x] QUIC tunnel with pinned certificates, pairing codes, real client addresses, outgoing connections from the gateway
- [x] Gateway program for a VPS: public mail and web ports, 421 while the server is away, mail ports only outwards, connection limits, systemd service with install script
- [x] Server side: pairing from the configuration, connections through the tunnel, outgoing mail only through the gateway
- [x] Setup assistant: detect a home connection (public address, Spamhaus PBL, reverse DNS, port 25, provider), recommend the gateway, pair with the code, DNS records with the gateway's addresses
- [x] Gateway in the health overview and under Server → Setup, notes on providers that block port 25
- [ ] Cloudflare button for the host name's A/AAAA records with the gateway's addresses
- [ ] Tried on a real VPS with a test instance behind it

## Later

- [ ] Sieve filters and ManageSieve
- [ ] JMAP Calendars and Contacts
- [ ] OAuth 2 / OpenID Connect provider for mail apps; login via external OIDC or LDAP
- [ ] Migration assistant (IMAP import from the old provider)
- [ ] Groups, shared mailboxes, masked addresses
- [ ] Settings sync for the UwUMail apps, send later and snooze on the server
- [ ] Web Push / UnifiedPush, sender pictures from the server
- [x] Nightly backups to SFTP: deduplicated, encrypted by default, 7/4/6 retention, full restore from the command line
- [ ] Restore per mailbox in the portal, backups to S3 or a mounted folder
- [ ] A calmer view of the admin panel for people who only want the traffic light
- [ ] Admin alerts, statistics, Prometheus metrics
- [ ] Sending TLS reports to other domains, DANE
