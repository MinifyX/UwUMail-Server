# Security audit — server and webmail, 0.16.0

The fourth pass over the whole stack, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md) and
[security-audit-2026-09-18.md](security-audit-2026-09-18.md). Everything built since then is in
scope — groups, shared mailboxes, masked addresses and the new masked-only domains, moving from
another provider, OAuth, Web Push, backups to S3 and folders, metrics — and so is everything older.

Scope: `uwumail-smtp`, `uwumail-imap` (with ManageSieve), `uwumail-jmap`, `uwumail-dav`,
`uwumail-store`, `uwumail-web` and `web/`, `uwumail-backup`, `uwumail-server` (CLI, fetch, moving,
restore), `uwumail-gateway`, `uwumail-tunnel`, `deploy/`, `docker/`, `scripts/`, and the webmail
(`UwUMail-Webmail`) with the headers the server serves it with.

Done with Claude, not an independent firm: a careful sweep by area, each finding traced through the
code, then a regression test that fails before the fix. Not a certificate. No exploit code or attack
payloads are written down here; the analysis says what was wrong and the diffs show how it was fixed.
Nothing was run against a production server.

The rule for this release: everything **Medium and above is fixed** in 0.16.0; Low and
Informational findings are listed here and fixed where the fix came along with another one.
0.17.0 fixed the open Low findings of the server, the gateway and the root helpers; each says how
below.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 1 | 1 | 0 |
| High | 11 | 11 | 0 |
| Medium | 30 | 29 + 1 partly | 0 (SMTP-3 partly, see there) |
| Low | 23 | 21 (17, and the rest of GW-4, in 0.17.0) | 2 (WEBMAIL-2, WEBMAIL-3) |
| Informational | 26 | 6 (PLAT-13 with GW-8 in 0.17.0) | 20 |

Almost every serious finding is the same kind: **one input takes the whole server down**. The server
is built with `panic = "abort"`, so a panic anywhere — a string sliced at a byte that is not a
character boundary, a recursion with no depth limit — ends SMTP, IMAP, JMAP and the portal for
everyone, and a message that crashes the server before it is stored crashes it again at every retry
of the sending server. The others are memory and CPU bombs: a small input that makes the server
allocate or compute far more than it should. None of the findings is a way into someone else's
mailbox from the outside; the ones between users (reused account ids, keyword injection, `mail`-only
tokens reaching calendars) are fixed.

What changed for good, beyond the single fixes:

- **One guard on the shape of every message** (`uwumail_store::mime_limits`): before any full parse,
  a cheap walk counts nesting (64 levels), parts (5,000) and header fields (20,000). SMTP, fetched
  mail, IMAP APPEND, JMAP import and create, moving and restore all go through it
  ([configuration.md](configuration.md#limits-on-the-shape-of-a-message)).
- **One login limiter for every protocol**, counted before the password check, and a server-wide
  cap on password checks running at once ([deployment.md](deployment.md)).
- **Account, app-password and OAuth-grant ids are never handed out twice**, and long-lived
  connections (IMAP, ManageSieve, JMAP WebSocket and event stream) end with their login.
- Per-request bounds for JMAP mail methods, result references and uploads
  ([jmap-clients.md](jmap-clients.md#limits)); per-client caps and deadlines for SMTP and IMAP.

## Critical and High

| ID | Severity | What was wrong | Fix |
| --- | --- | --- | --- |
| SMTP-1 | Critical | A message nested deeply as `message/rfc822` overflowed the stack when the parsed message was dropped. Anyone on port 25 could abort the server, again at every retry, and a fetched mailbox holding such a message crashed it at every fetch. | `mime_limits`: a nesting, part and header count before any full parse, for every way a message comes in. |
| SMTP-2 | High | The spam filter's text and HTML readers had quadratic loops over attacker text and no bound; one message could keep a core busy for hours. | Links found in one pass, `<style>`/`<script>` skipped without copying, entity lookahead bounded, at most one examination per core. |
| WEB-1 | High | Logins without an account could start any number of Argon2 checks at once on the pool the database runs on: ~19 MiB each, every database call waiting behind them. | Server-wide cap on concurrent password checks with "try again later"; checks in progress count against the network. |
| PROTOCOLS-1 | High | JMAP `bodyStructure` recursed without a depth limit. | Stops at 32 levels (`subParts: null` below). |
| PROTOCOLS-2 | High | `Email/set` create copied a referenced blob once per part naming it; a few kB of JSON could use all memory. | At most 1,000 parts and 50 MB counted per reference; blobs loaded once and borrowed. |
| PROTOCOLS-3 | High | An invitation from anyone could store a start date that later crashed `itip::summary` when the invitation was declined or deleted. | Digits checked before the date is cut. |
| PROTOCOLS-4 | High | CalDAV `utc_time` sliced text by byte; one time-range REPORT crashed the server. | Exactly 8 and 6 ASCII digits required. |
| PROTOCOLS-5 | High | One calendar change mailed every attendee separately, past `max_recipients`. | One change may mail at most `max_recipients` outside people and 1,000 in all; refused before storing. |
| PROTOCOLS-14 | High | An IMAP APPEND date panicked the parser before login; a hostile server behind a fetch account triggered it at every poll. | Numbers read as digits before anything is cut; in the fuzz corpus. |
| PLAT-1 | High | A short `LIST` answer from a user-chosen IMAP server (fetch, moving) crashed the server, and again after every restart. | LIST answers read by a checked `list_entry`; the answer readers get a randomized test. |
| PLAT-5 | High | Symlinks planted in a folder backup target were followed: pruning could delete live mail, a test write could overwrite the database. | The folder target works from a directory handle and never follows links (`O_NOFOLLOW`); a backup that would write through one stops with an error. |
| PANIC-2 | High | JMAP result references copied earlier responses without a budget, and `Core/echo` handed them on; one small request could grow memory without limit. | 10 MB of references per request and 16 per call. |

## Medium

| ID | What was wrong | Fix |
| --- | --- | --- |
| SMTP-3 | DANE could be bypassed: MX hosts came from the resolver that does not check DNSSEC, so a forged "no MX" skipped DANE. | MX asked of the validating resolver; a signed answer decides. **Partly:** a validating lookup that fails or times out still falls back to delivery without DANE, as documented in [tls-reports.md](tls-reports.md). |
| SMTP-4 | Millions of tiny headers or parts cost 10–40× their size in memory. | Part and header-field counts in `mime_limits`. |
| SMTP-5 | One client could hold all SMTP connection slots with a byte every few minutes. | `smtp.max_connections_per_client` (20 per address or IPv6 /64) and session deadlines. |
| SMTP-6 | An encoded comma list in a domain's TLS-RPT record gave any number of report recipients. | One address per `mailto:`, at most five report addresses. |
| SMTP-7 | Personal word lists had no total memory limit, and every edit recompiled every list under a lock. | Memory budgets per person, domain and server; only changed lists recompile, without the lock. |
| PROTOCOLS-6 | JMAP read request and upload bodies before checking the login. | Login first, body after. |
| PROTOCOLS-7 (= STORE-2) | JMAP uploads counted against no quota; one account could fill the disk. | Uploads of the last 24 h may take 1 GiB and never more than the quota. |
| PROTOCOLS-8 | The per-login throttle protected only the portal. | One limiter for portal, IMAP, ManageSieve, SMTP, JMAP, DAV and `/jmap/token`. |
| PROTOCOLS-9 | `Email/parse`, `Email/get` and queries lacked per-request work limits. | Id, filter and sort limits; parsing off the async workers within the request's time budget. |
| PROTOCOLS-10 | Scheduled mail still went out after the account was disabled, trashed, or its login revoked. | Held mail remembers its credential and is cancelled when that login ends. |
| PROTOCOLS-11 | `mail`-only app passwords and OAuth tokens reached calendars and contacts over JMAP. | Those need the `dav` scope now. |
| PROTOCOLS-12 | A calendar move from another provider held unbounded remote data in memory. | 128 MiB and 100 collections per move, one move per account at a time. |
| PROTOCOLS-13 | One free-busy request expanded full calendars up to 100 times on the async workers. | Each calendar once, off the workers, 10 s at most. |
| PROTOCOLS-15 | Repeating one IMAP FETCH item multiplied memory. | Repeats answered once, at most 100 items, answers streamed. |
| PROTOCOLS-16 | Keywords stored over JMAP were not checked; a grantee could inject fake IMAP responses into other users' sessions. | Keywords must be IMAP atoms; IMAP output skips anything else; the migration removes bad ones. |
| PROTOCOLS-17 | IMAP connections could stay open before login indefinitely. | 60 s per command and 3 minutes in all before login (as ManageSieve since S-47). |
| PLAT-2 (+ WEB-4) | Fetch and moving checked that one resolved address was public, then connected by name. | Resolve once, every address public, connect to a checked one (`Source::remote`). |
| PLAT-3 | The IMAP client's memory limit counted bytes received, not memory held. | Charged by the memory an answer holds. |
| PLAT-4 | A hostile provider could stall the move worker for everyone (handshake and LOGIN outside the timeout). | One deadline over the whole turn. |
| PLAT-6 | A hostile S3 endpoint could return an endless listing, or pin a thread with crafted XML. | Listing budget, linear XML unescape, deadline per answer. |
| STORE-1 | Row ids of purged accounts were handed to new ones; an old IMAP, ManageSieve or WebSocket session then acted on the new account. | Ids are never reused (`id_high_water`); long-lived connections check their login before every command. |
| STORE-3 | Turning a person into a service or shared mailbox kept their forwarding, fetch accounts, active Sieve script, moves and calendar subscriptions. | Removed or switched off on conversion; masked addresses disabled ([groups.md](groups.md)). |
| GW-1 | Failed tunnel handshakes counted as refusals, so forged packets or a CGNAT neighbour could lock the paired server out of its gateway. | Only finished handshakes count; the paired server and its trusted address are never locked out. |
| GW-2 | The gateway installer's report, run as root, read a file the gateway user could replace with a symlink and printed it into a log that user can read. | Links skipped, size bounded, only address-shaped fields printed. |
| GW-3 | `scripts/deploy-gateway.sh` could take the binary of a fork's pull-request CI run and run it as root on the VPS. | Only a push run of this repository whose commit is on `main`. |
| PANIC-4 | IMAP SEARCH had no limit on keys and matched on the async workers. | 100 keys, header fields read once, matching off the workers. |
| PANIC-5 | Reading a website for a sender picture was quadratic on pages with unclosed `<link` tags. | Tags read to the next `<`/`>`, at most 200. |
| PANIC-6 | `UID EXPUNGE` and moves compared lists in O(n·m) inside the write transaction. | Sets and one pass. |
| WEBMAIL-1 | Four regular expressions in the webmail took quadratic time on attachment names and links; opening a mail could freeze the tab. A fifth (`isEmail`, on `List-Unsubscribe`) was found while fixing. | Linear loops; webmail 0.11.0. |

## Low (open unless noted)

- **WEB-2** — anyone may register OAuth apps (30 an hour per network); two networks can keep the
  table full so no new OAuth sign-in works. App passwords are unaffected. **Fixed in 0.17.0:** apps
  nobody ever allowed in go after a day, and a full table forgets the oldest of them instead of
  refusing.
- **WEB-3** — the OAuth authorize endpoint redirects errors to the registered URI of any
  self-registered app without a click (an open redirect for logged-in portal users). **Fixed in
  0.17.0:** errors go back by themselves only to an app allowed in before or one on the device;
  otherwise the portal shows them (RFC 9700 section 4.11.2).
- **WEB-4** — fixed with PLAT-2.
- **WEB-5** — granting an OAuth app and adding an external forwarding address do not ask for the
  password again, as app passwords do. **Fixed in 0.17.0:** both go through the same confirmation
  as app passwords (the password unless the login is younger than ten minutes).
- **SMTP-8** — mail from outside forging one of our own domains is forwarded without sender
  rewriting; matters only with that domain at DMARC `p=none`. **Fixed in 0.17.0:** a From that did
  not pass DMARC and aligns with the forward's domain is not sent to other servers, and an unproven
  sender of our own domain gets SRS.
- **SMTP-9** — outgoing mail passes a lone CR through (SMTP smuggling towards lax receivers).
  **Fixed in 0.17.0:** every lone CR or LF leaves as CRLF; submitted mail is signed after the same
  change.
- **SMTP-10** — fetched mail without a readable provider verdict skips the two-`From` and header
  checks. **Fixed in 0.17.0:** they run on every incoming message, whatever else can be checked.
- **PROTOCOLS-L1** — fixed with STORE-1: a WebSocket ends with its credential.
- **PROTOCOLS-L2** — a shared account shows the owner's private mail activity through
  `/changes` and push states (no content). **Fixed in 0.17.0:** a sharee's state, changes and
  pushes move only with the shared mailboxes and the sharing (migration 0049); IMAP HIGHESTMODSEQ is
  per mailbox.
- **PROTOCOLS-L3** — push subscriptions make the server POST to any public host and port.
  **Fixed in 0.17.0:** https on port 443 only, checked when subscribing and before each push.
- **PROTOCOLS-L4** — any local user can read any local free/busy, masked addresses included, which
  links a masked address to its owner for local users. **Fixed in 0.17.0:** only a person's own
  addresses count, never masked ones, and only for the asker's domains or who shares a calendar
  with them.
- **PROTOCOLS-L5** — control characters from other accounts can break an owner's CalDAV sync.
  **Fixed in 0.17.0:** entries are stored without them, and DAV XML leaves out what XML cannot
  carry.
- **PLAT-7** — single-mailbox restore opens a hostile unencrypted snapshot's database without
  limits (admin-chosen snapshot). **Fixed in 0.17.0:** values of at most 64 KB, real tables only,
  capped folders and messages, interrupted after ten minutes.
- **PLAT-8** — changing the SFTP backup host keeps the stored password without asking again.
  **Fixed in 0.17.0:** the stored password is kept only for the same host, port and user.
- **STORE-4** — fixed: push subscriptions go when their app password or OAuth app is revoked.
- **GW-4** — terminal escape sequences from helper reports reach root's terminal; fixed in the
  installer, open in `deploy/gateway/hardening/helper` and `deploy/host/helper`. **Fixed in
  0.17.0:** both helpers show only plain characters of a report, and counts only as digits.
- **GW-5** — unauthenticated tunnel peers get the full stream budget and unlimited handshakes.
  **Fixed in 0.17.0:** one stream and 256 KB until paired; 16 handshakes of strangers at once, 2 at
  once and 20 a minute per network.
- **GW-6** — twenty IPv6 /64s can take all 1,000 public connection slots at the gateway.
  **Fixed in 0.17.0:** an IPv6 /48 may hold 100 at once (`max_connections_per_ipv6_site`).
- **GW-8** — the OpenVPN file filter in the host helper can be bypassed (code in the gluetun
  container only, not the host). **Fixed in 0.17.0:** helper and portal allow only the directives of
  a connection and inline keys.
- **WEBMAIL-2** — the invitation card trusts any mail naming an event's UID, including a
  `METHOD:CANCEL` the server itself would not accept from that sender.
- **WEBMAIL-3** — W-23 (calendar links without the link check on middle click) is now reachable by
  any sender, since invitations land in the calendar by themselves.
- **PANIC-7** — after a share is narrowed, the grantee's next IMAP command still runs with the old
  rights once. **Fixed in 0.17.0:** the rights are read again before every command on the selected
  mailbox.
- **MD-1** — upgrading turns a domain that was open for masked addresses into "own domain" for its
  own users; a domain that only carried masked addresses then offers them to nobody until the admin
  makes it masked-only and assigns it (see the 0.16.0 changelog). **In 0.17.0:** kept as the admin's
  decision (a change by itself could override what was set on purpose since), but the health
  overview names every such domain and links to where it is fixed.

## Informational

- WEB-6 / WEBMAIL-4 — `/jmap/download` serves the type the caller asks for, so `script-src 'self'`
  could be met with uploaded bytes if an HTML injection ever appeared (none known).
- WEB-7 — LDAP login allows any address when the directory lists none for the person.
- WEB-8 — "Test LDAP" sends the saved bind password to an unsaved directory URL (extends W-6).
- WEB-9 — the OIDC callback has no throttle.
- SMTP-11 — outgoing delivery connects to MX hosts on private addresses.
- SMTP-12 — a local user can plant an invitation naming a colleague as organizer.
- PROTOCOLS-I1 — the JMAP principal directory is server-wide, across unrelated hosted domains.
- PROTOCOLS-I2 — advertised `maxConcurrentUpload`/`maxConcurrentRequests` are not enforced.
- PROTOCOLS-I3 — JMAP/DAV blocks are not reported to the gateway blocker.
- PROTOCOLS-I4 — one account's slow calendar feeds can delay everyone's refreshes.
- PROTOCOLS-I5 — DAV sharing accepts services as grantees, mailbox sharing does not.
- PLAT-9 / STORE-6 — the sealing key lives in the same database, and backup credentials, TOTP
  secrets and DKIM keys are stored unsealed; the comment in `web/src/settings.rs` overstated this.
- PLAT-10 — `write_private` briefly exposes key material and follows a planted symlink.
- PLAT-11 — `import imap --password <value>` still takes a password on the command line.
- PLAT-12 — admin-only S3 and folder target checks are weaker than they look.
- PLAT-13 — fixed with GW-8 in 0.17.0: the OpenVPN filter is an allowlist now.
- GW-7 — the VPS can reach `/metrics` when it is protected by network only.
- GW-9 — a check-then-read race in the host helper, harmless with today's permissions.
- GW-10 — the gateway's downgrade check lets a pre-release of the running version through.
- PANIC-I1 — fixed: saturating arithmetic where overflow checks would panic.
- MD-2 to MD-5 — fixed while building masked-only domains (default fallback order, logging on
  domain removal, a stray Save button, per-request queries for the session state).

## What held up

- **Authorization:** all of roughly 230 portal handlers use the right admin or login check, CSRF is
  enforced for every non-GET request in the session extractor, and every id taken from a client is
  scoped to its owner — in the portal, JMAP, IMAP, DAV and the store.
- **SQL:** every `format!` in SQL inserts only constant identifiers or bound placeholders.
- **Secrets:** Argon2id with a dummy hash for unknown logins; hashed web sessions, links, recovery
  codes, OAuth codes and tokens; single-use TOTP steps; PKCE and push verification compared in
  constant time; OAuth refresh rotation with reuse detection.
- **Mail authentication:** relaying, send-as checks (aliases, groups, masked and shared addresses),
  DMARC on port 25, closed groups, backscatter limits, Sieve limits and outbound TLS still hold, and
  the earlier fixes S-1 to S-4 and S-16 are intact.
- **Tunnel:** pairing tokens, certificate pinning, the control channel's narrow message set, the
  outbound proxy's IP-literal rule and the host helper's fixed verbs.
- **Webmail:** the sandboxed reader frame and its CSP, the link and mailto checks, attachment
  handling, the service worker (no cache, no fetch handler, login checked before each call), and the
  demo backend kept out of production.
- **Masked-only domains (new):** kind and policy changes are admin-only and logged; every path that
  puts an account, alias, group, forward, catch-all or send-as grant on a domain checks its kind in
  the same write transaction; delivery and send-as for existing masked addresses ignore the policy.
