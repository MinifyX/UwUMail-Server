# Security review — the fetch feature, the traps, and a re-read of the whole server, 20 September 2026

The sixth pass, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md),
[security-audit-0.3.0.md](security-audit-0.3.0.md),
[security-audit-0.4.0.md](security-audit-0.4.0.md) and
[security-audit-0.5.0.md](security-audit-0.5.0.md). This one is the security work for the planned
**0.5.2** release. Three things were looked at:

- **Everything new since 0.5.0** — above all the **fetch feature** (the "Sammeldienst": pulling
  mail from foreign mailboxes over IMAP and sending as the fetched address through the provider's
  SMTP), which shipped dormant in 0.5.0 and now has a caller, plus the **spam traps** that take mail
  and teach the filter, and the greylist-hold path.
- **The gateway and the tunnel** (`crates/uwumail-gateway`, `crates/uwumail-tunnel`,
  `deploy/gateway`), which 0.5.0 did not change and which had not been re-read against the
  "the VPS is only partly trusted" threat model since it was built.
- **A fresh read of the rest of the server** — SMTP receive/submission, DKIM/SPF/DMARC/SRS,
  JMAP auth and sessions, the store, the container and the release chain — looking for anything the
  earlier passes missed rather than only re-checking their fixes.

The whole of [UwUMail-Webmail](https://github.com/MinifyX/UwUMail-Webmail) was reviewed in the same
sweep; its findings are in that repository's own report,
[UwUMail-Webmail/docs/security-audit-2026-09.md](https://github.com/MinifyX/UwUMail-Webmail/blob/main/docs/security-audit-2026-09.md).

The threat model that matters most here is twofold: the **anonymous internet sender on port 25**,
and — new for the fetch feature — an **ordinary logged-in account attacking another account or the
admin**, because the fetch feature is the first place where a normal user's own configuration
decides how the server sends mail.

Done with Claude, not an independent firm. An honest sweep, not a certificate. Nothing was run
against the production server, and nothing was changed on any machine.

## How this was verified

This audit ran as a large fan-out: recon, one finder per attack surface, then an adversarial
verification pass. Every Critical/High/Medium/Low finding was then re-read by an independent
reviewer whose job was to **refute** it — reading the exact code, the callers and callees, and (for
the DMARC findings) the vendored `mail-auth 0.13.2` and `mail-parser 0.11.9` sources — applying
three lenses: reading-correctness, exploitability, and existing-mitigation. The Critical and High
findings were checked by **two** independent reviewers each.

**Outcome: all 44 verified findings were confirmed; none was refuted.** The verification also
produced these corrections, which are already applied below:

- **S-4** — one of the two example triggers was wrong and has been removed. `From :` (a space before
  the colon) does **not** bypass DMARC: both mail-auth and mail-parser fold it back to a `From`
  header. Only the **colon-less line above the `From`** produces the parser split. The finding
  stands on that path.
- **Severity revised up after verification:**
  - **S-22 Low → Medium** — authentication confusion into a *recreated* account is operationally
    plausible (offboard-then-recreate; an auto-polling client re-presents the old password for
    free).
  - **S-25 Low → Medium** — the unbounded IMAP literal aborts the **whole in-process server**
    (`panic=abort`) and crash-loops on restart; it is remotely triggerable by a hostile/compromised
    fetch provider, not just a CLI DoS.
  - **G-5 Low → Medium** — a clean gateway-user → root escalation (arbitrary `chmod 0644` /
    truncate / append as root, e.g. reading `/etc/shadow`), the same primitive class as S-12.
- **Kept as rated, with a noted argument for higher:** **S-2** (High; a reviewer argued Critical
  because the forged mail is DKIM-signed with the local domain's real key), and the borderline
  Low/Medium items **S-8/S-9** (silent permanent mail loss), **S-13**, **S-19**, **S-26**, **S-27**.
- **Stale line numbers corrected:** **G-1** (`state.rs:68-84`, not `216-232`); the webmail
  **W-8/W-9** references are corrected in that repository's report.

Every finding says how it was established under "Evidence". Almost all were established by reading
the code, not by running a live exploit — the rules for this test allowed only non-invasive checks
against the test VM (no fuzzing, brute-force, DoS or outbound mail), so nothing was proven by
sending a malicious message end to end. Where the test VM was used it was for reading configuration
and confirming which listeners answer, with throwaway accounts on the existing test domain that were
removed afterwards.

## Summary

| Severity | Found | Fixed | Accepted / deferred |
| --- | --- | --- | --- |
| Critical | 1 | 1 | 0 |
| High | 4 | 4 | 0 |
| Medium | 11 | 11 | 0 |
| Low | 19 | 17 | 2 |
| Informational | 11 | 7 | 4 |

46 findings. Every Critical, High and Medium is fixed in 0.5.2, and all but six of the rest; the six
are deferred or accepted, listed under [What was not fixed in
0.5.2](#what-was-not-fixed-in-052). (Medium/Low counts reflect the post-verification revisions in
[How this was verified](#how-this-was-verified): S-22, S-25 and G-5 moved Low → Medium.)

## What was not fixed in 0.5.2

Six findings are not code-fixed in this release; each is Low or Informational, with a reason:

- **S-23 (Low) — deferred.** Requiring the admin's password again for account-takeover actions is a
  cross-stack change (the four handlers plus wiring the portal's existing confirm-password dialog to
  the People page). It is deferred to a change that can be tested against the running portal; a stolen
  live admin session can already perform every admin action, so this only removes the last speed bump.
- **S-29 (Low) — deferred to the operator.** Pinning the build/prep Docker stages by `@sha256`
  needs the real current digests fetched from the registry; a made-up digest would break the build.
  Fetch and pin them with a CHANGELOG line (the runtime base is already digest-pinned).
- **S-32 (Info) — accepted.** Relaying a null-sender SRS return is standard MTA behaviour; dropping
  Junk-scored returns is a hardening to add with flow tests, not a defect.
- **S-33 (Info) — accepted.** A trap learning from what it catches is the feature; a per-trap
  per-day learning cap is an opt-in hardening (a store counter) best added and measured on its own.
- **S-34 (Info) — accepted.** A bounce naming the forwarding target is standard MTA behaviour;
  reporting the forwarder's own address instead threads the forwarder identity through the bounce
  path and wants dynamic testing.
- **S-37 (Info) — record corrected here, not changed in compose.** The `no-new-privileges` clause
  belongs to the ClamAV service, not the server's; it was **not** added to the server service
  because the binary gains `NET_BIND_SERVICE` from a `setcap` file capability, which
  `no_new_privs` disables on exec — that would stop it binding 25/80/443. There is no concrete
  escalation today (distroless, no setuid), so the record is simply corrected. Almost all
of the Critical/High/Medium weight sits in the **fetch feature**: the one place in the server where
a user's own row decides who may send as an address and where the server connects to send. The
DMARC-bypass findings on the receive path are independent of it and equally worth fixing before the
server takes real mail. IDs use the scheme from the test brief: `S-…` server, `G-…` gateway/tunnel,
`R-…` release/CI.

## Threat model

The attackers considered:

- **The anonymous internet sender** on port 25 (directly or through the gateway): chooses the whole
  message — envelope, every header, the body, attachments — and wants to bypass DMARC, be trusted,
  poison the filter, or make the server send a bounce or auto-reply somewhere.
- **A logged-in account** (portal/JMAP/webmail session, no admin): wants to read or send another
  account's mail, impersonate the admin internally, or use the server's network position to reach
  hosts it should not.
- **The mail provider** a user fetches from, and anyone on the path to it.
- **The compromised VPS** running the gateway, against the home server (the gateway's stated model:
  "trust the VPS like the server" is *not* the model — the VPS is only partly trusted).
- **A process that already runs as the container's unprivileged user** after some other compromise,
  trying to reach the host.
- **The release supply chain.**

## Findings

### S-1 · Critical · Outbound mail is routed by the envelope address alone, so a user can intercept or send as another account

`crates/uwumail-smtp/src/outbound.rs:185-208` → `crates/uwumail-store/src/fetch.rs:573-613`;
schema `crates/uwumail-store/src/migrations/0027_fetch_accounts.sql`

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:N` (Critical)
- **Attacker & preconditions:** any logged-in portal user, no admin. For interception, the victim
  (another local account, e.g. the admin) sends mail to a remote recipient; the attacker owns any
  host with a valid certificate that answers SMTP with STARTTLS + AUTH.
- **Impact:** the fetch feature lets a user register a "fetched mailbox" for an address and turn on
  "send as this address" with their own SMTP host and credentials. The queue then decides the
  outbound route from the **return path of the message alone**: `sender_route` calls
  `fetch_sender(return_path)` with the envelope address and never consults the message's own
  `account_id`. `fetch_sender`'s SQL selects `WHERE address = ?1 AND send_enabled = 1 AND
  smtp_host <> ''` with no account scoping and takes the first matching row. The schema permits the
  collision: `UNIQUE (account_id, address)` lets two different accounts register the same address.
  So:
  - **Interception:** if the attacker registers the victim's own address and their row is chosen,
    every message the victim submits with that return path is delivered to the *attacker's* SMTP
    host, authenticated with the attacker's credentials after a real TLS handshake, and accepted
    with `250` — the victim sees "delivered", and the attacker receives the original,
    DKIM-signed and replayable.
  - **Sending as the victim:** in the reverse collision the attacker submits as the victim's
    address and the mail leaves through the *victim's* provider with the victim's stored
    credentials — full impersonation of the victim's external identity.

  `docs/fetch.md:125-128` promises "the one place that decides who may send as which address asks
  for the account, not only for the address" — the routing place does not.
- **Evidence:** verified by hand. Read `outbound.rs:185-208` (`sender_route` passes only
  `return_path`; `QueuedMessage.account_id` in `crates/uwumail-store/src/queue.rs:57` is never
  used), `fetch.rs:573-613` (the SQL has no `account_id`, `query_row` takes the first row) and
  `0027_fetch_accounts.sql` (the unique key is per `(account_id, address)`, not per address). Not
  run as a live exploit (no outbound mail allowed in this test).
- **Fix:** scope the route to the sender's account. Make `fetch_sender` take `(account_id, address)`
  and add `AND account_id = ?`; call it from `sender_route` with `message.account_id`, and skip the
  sender route entirely when `account_id` is `None` (system mail, bounces). Additionally refuse a
  fetched address whose domain is hosted here or resolves to a local account (see S-2), so the
  collision cannot be created in the first place.
- **Regression test:** `cargo test -p uwumail-store fetch`: two accounts both register a fetch row
  for the same address with `send_enabled`; assert `fetch_sender(a_id, addr).host == a_host` and
  `fetch_sender(b_id, addr).host == b_host`, and that `sender_route` for a message with
  `account_id = a` never returns b's host.

### S-2 · High · An unverified fetched-mailbox row makes any account the owner of any sender address

`crates/uwumail-store/src/extras.rs:84-92`, `crates/uwumail-store/src/fetch.rs:372-380`

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:C/C:N/I:H/A:N` (High)
- **Attacker & preconditions:** any logged-in portal user. Two API calls (create a fetch account,
  then enable sending) with no proof that the address is theirs; the fetch itself can stay disabled
  so nothing ever connects to a provider.
- **Impact:** `account_owns_address` grants ownership straight from the row:
  `OR EXISTS (SELECT 1 FROM fetch_accounts f WHERE f.account_id = ?1 AND f.address = ?5 AND
  f.send_enabled = 1 AND f.smtp_host <> '')`. After registering, say, `admin@<hosted-domain>` (or
  another user's alias, or any external address) and switching `send_enabled` on with any non-empty
  host, the attacker passes ownership for `MAIL FROM`, `From`, `Sender` and `Resent-*`. Mail to
  local recipients is ingested into their inboxes **as the admin, DKIM-signed with the hosted
  domain's real key**, with a trace header that names no login — internal phishing
  indistinguishable from a real admin mail. For remote recipients the message is DKIM-signed and
  (via S-1) handed to the attacker's own host to relay or replay with an aligned DMARC result.
  `create_fetch_account` runs only `normalize_address()` and `check_host()`; there is no
  `is_local_domain` refusal, no recipient-resolution check, and no test that the account controls
  the address.
- **Evidence:** verified by hand — read the `EXISTS` clause in `extras.rs:84-92` and the absence of
  any local-domain/ownership check in `fetch.rs:372-380`.
- **Fix:** in `create_fetch_account`/`update_fetch_account` refuse an address whose domain is hosted
  here (`Store::is_local_domain`) or that resolves to a local account, alias or forwarding target;
  require proof of control (a recorded successful fetch of that address) before `send_enabled` may
  be turned on.
- **Regression test:** `cargo test -p uwumail-store fetch`: `create_fetch_account` for an address of
  a hosted domain and for another account's alias → `Err(Invalid)`; `update` with
  `send_enabled=true` before any successful fetch → `Err`.

### S-3 · High · DMARC bypass on the receive path with two `From` headers

`crates/uwumail-smtp/src/checks.rs:69` (via mail-auth 0.13.2)

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:H/A:N` (High)
- **Attacker & preconditions:** anonymous internet sender on port 25; needs only a domain of their
  own with SPF/DKIM so the message does not lose points for being unauthenticated.
- **Impact:** a message with `From: attacker@evil.example` (first) and `From: ceo@bank.example`
  (last, a `p=reject` domain) is never judged by DMARC — mail-auth exempts a message whose multiple
  `From` headers span several domains (`DmarcOutput::default`, no record), so it is neither refused
  nor quarantined nor given DMARC-fail points. The server's own checks run against `evil.example`
  (DKIM passes → `from_verified`/`sender_verified` true), while the store, JMAP, webmail and desktop
  client all display `ceo@bank.example`. `enforce_dmarc_reject` and the `p=reject` path are
  bypassed for any display `From`.
- **Evidence:** verified by hand against mail-auth 0.13.2 (`src/dmarc/verify.rs:54-67` exempts the
  multi-domain case; `src/common/message.rs:249-256` collects every `From` header). Not run live.
- **Fix:** in `receive()`, before `checks::verify`, refuse a message whose header block carries more
  than one `From` header (`550 5.6.0`, per RFC 7489 §6.6.1), and treat a `From` with addresses in
  several domains as a DMARC fail for every domain.
- **Regression test:** `flow.rs`, like `forged_bank_mail` but with DATA carrying two `From` lines in
  different domains under a `p=reject` policy → expect `550`.

### S-4 · High · DMARC bypass through a header parser differential

`crates/uwumail-smtp/src/checks.rs:48` (via mail-auth 0.13.2)

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:H/A:N` (High)
- **Attacker & preconditions:** anonymous internet sender on port 25 (also applies to fetched
  mail); one malformed line in the header block.
- **Impact:** a **colon-less line** (such as `X\r\n`) placed above the `From` header makes
  mail-auth's `scan_field` treat it as the end of the header block, so a following `From` is never
  parsed and DMARC is not evaluated — while mail-parser continues past that line and still parses
  (and the stored/displayed message still shows) `ceo@bank.example`. With the attacker's own domain
  in `MAIL FROM` (SPF pass) the spam score is near zero and the message reaches the inbox displaying
  a `p=reject` sender. *(Verification correction: a `From :` line with a space before the colon does
  **not** bypass — both parsers fold it back to a `From` header. Only the colon-less-line variant
  works, and it needs CRLF, which every real MTA/DATA uses.)*
- **Evidence:** verified against both vendored parsers — mail-auth `common/headers.rs` (`scan_field`
  returns end-of-headers on a colon-less CRLF line) and mail-parser `parsers/header.rs`
  (`parse_header_name` returns `None` on that line but the loop continues). Not run live.
- **Fix:** validate the header block once in `receive()` with the server's own splitter before any
  check — every line up to the blank line must be a token-only `field-name:` or a folded
  continuation, otherwise `550 5.6.0 Malformed header` (fetched mail → Junk).
- **Regression test:** `flow.rs` variant with a colon-less line above `From: security@bank.test`
  (CRLF) under `p=reject` → refused; and a control with `From : …` (space before colon) that is
  still DMARC-evaluated normally.

### S-5 · High · Behind a trusted relay, the sender address is taken from the HELO literal

`crates/uwumail-smtp/src/relay.rs:81-93`

- **CVSS 3.1:** `AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:H/A:N` (High)
- **Attacker & preconditions:** anonymous internet sender, when the server runs behind a mail server
  listed in `smtp.trusted_relays` — the documented "next to a mail server" deployment
  (`deploy/next-to-mailserver/uwumail.toml:23`) — or behind a second UwUMail. No login; one SMTP
  session to the relay.
- **Impact:** `parse_received_from` takes the client address from the **first** `[...]` bracket in
  the relay's `Received` line, which is the HELO when the HELO is an address literal (Postfix writes
  `from HELO (rDNS [IP])`). The attacker therefore chooses the address every sender check uses:
  `EHLO [<IP in the spoofed domain's SPF>]` yields SPF pass → DMARC pass by alignment (allow-list
  entries match, reputation is booked on the spoofed domain, a clean message is auto-learned as
  ham server-wide); `EHLO [10.0.0.1]` makes the scorer skip everything (private address); an
  unbalanced bracket makes the client `None` and the message is delivered with no scoring at all.
- **Evidence:** verified by hand — read `relay.rs:81-93` (`from.find('[')` takes the first bracket)
  against how Postfix formats the header. Not run live.
- **Fix:** take the address from the parenthesised comment — the last `[...]` before ` by ` — never
  the first bracket; treat a HELO containing `[`, `]`, `(` or `)` as unusable; sanitise the HELO to
  a hostname when writing the server's own `Received`.
- **Regression test:** `flow.rs` behind `trusted_relays=[127.0.0.1]`, hand in
  `Received: from [203.0.113.7] (unknown [198.51.100.9]) by relay.local …` and assert the client
  address used is `198.51.100.9`, not `203.0.113.7`.

### S-6 · Medium · Fetched mail trusts a sender-written `Authentication-Results`, and a private client-ip switches checks off

`crates/uwumail-smtp/src/fetched.rs:124-131`

- **CVSS 3.1:** `AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:H/A:N` (Medium)
- **Attacker & preconditions:** anyone who can send mail to the victim's mailbox at a provider that
  does not add an `Authentication-Results` header with an `authserv-id` ending in the IMAP host's
  site (self-hosted Dovecot, many hosters; Microsoft writes none). The victim configured that
  mailbox under Fetch with `auth_serv_id` left empty.
- **Impact:** the first `Authentication-Results` in the message is trusted by name alone. A
  `From : strato.de; spf=pass smtp.mailfrom=ceo@bank.example client-ip=<IP in bank.example's SPF>`
  header (plus a matching `Return-Path`) makes UwUMail run SPF against the forged address → pass →
  DMARC pass by alignment, and `checked_ourselves` then suppresses every `PROVIDER_*` rule; a
  `client-ip=10.0.0.1` makes the scorer skip all rules, so even the provider's own junk-folder mail
  lands in the inbox with the server's own `Authentication-Results` claiming `spf=pass`.
- **Evidence:** code read of `fetched.rs:124-131` (`find` by header name, no position check). Not
  run live.
- **Fix:** trust an `Authentication-Results` header only if it stands above the provider's own trace
  (stop at the first `Received:`), and require the header to be the provider's by position, not by
  name; ignore `client-ip` for the "private → skip" shortcut on fetched mail.
- **Regression test:** `flow.rs` fetched test: a message whose first `Received` is the provider's and
  whose sender-written `Authentication-Results` claims `spf=pass` for a foreign domain is scored as
  unauthenticated.

### S-7 · Medium · The session-cookie reader accepts the un-prefixed name over HTTPS (session fixation)

`crates/uwumail-jmap/src/auth.rs:32-42`

- **CVSS 3.1:** `AV:N/AC:H/PR:N/UI:R/S:U/C:L/I:L/A:N` (Medium)
- **Attacker & preconditions:** anyone who can set a cookie for the portal host in the victim's
  browser without being the portal — a page on a sibling host under the same registrable domain, or
  a network attacker answering one plain-HTTP request when HSTS is not in force — and who has a
  portal account of their own.
- **Impact:** `session_cookie()` accepts both `__Host-uwumail` and the plain `uwumail` on every
  request regardless of transport, and returns the first match in header order. A planted
  `uwumail=<attacker token>` listed before the victim's `__Host-uwumail` silently binds the
  victim's browser to the attacker's session, so everything the victim does in the portal or webmail
  happens inside the attacker's account. The `__Host-` prefix was chosen precisely to exclude this;
  the reader undoes it.
- **Evidence:** code read of `auth.rs:32-42`; a test at `:292-300` pins the both-names behaviour.
- **Fix:** make the reader transport-aware — over HTTPS read only `__Host-uwumail`, over plain HTTP
  only `uwumail`. `ClientInfo.https` is already available at both call sites.
- **Regression test:** `session_cookie(&headers, https=true)` with
  `Cookie: uwumail=attacker; __Host-uwumail=victim` returns `victim`; with `uwumail=attacker` alone
  and `https=true` returns `None`.

### S-8 · Medium · A refused fetched message is still deleted or marked read at the provider

`crates/uwumail-server/src/fetch.rs:261-286`

- **CVSS 3.1:** `AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:H/A:N` (Medium)
- **Attacker & preconditions:** no attacker needed — DMARC rejects of legitimate list mail, virus
  false positives, score rejects, or a full local mailbox (`552`) all hit the same path. An attacker
  who can make the filter reject a message (or fill the victim's quota) triggers it deliberately.
- **Impact:** with `after_fetch = delete` the only copy of a refused or quota-blocked message is
  expunged at the provider: `Taken::Refused` falls through to the after-fetch block, which flags
  `\Seen` or `\Deleted` and expunges, and advances `last_uid` so it is never offered again. A full
  mailbox here therefore destroys mail at the provider. `docs/fetch.md:96-99` promises refused mail
  is "not deleted at the provider either, whatever the mailbox is set to".
- **Evidence:** code read of `fetch.rs:261-286` (`Taken::Kept | Taken::Refused(_) => {}` falls into
  the after-fetch block) and `inbound.rs:259-265` (every non-2xx/4xx becomes `Refused`).
- **Fix:** on `Taken::Refused` skip after-fetch entirely (do not flag/expunge; record the UID as
  refused so it is not re-fetched forever), and map the quota answer for fetched mail to "later" by
  checking the owner's quota before delivering, as RCPT does.
- **Regression test:** worker test against the in-crate IMAP server: a message the filter refuses
  with `after_fetch=Delete` must still exist at the provider after the run.
- **Changed after 0.5.2 (21 September 2026), at the operator's request.** The first half of this fix
  is deliberately turned back: a message the filter *refuses* — virus, blocked sender, rejecting
  DMARC policy, score over the limit — is now marked read or deleted at the provider by
  `after_fetch`, like one that arrived, because leaving it there filled fetched mailboxes with
  exactly the mail this server had already thrown out. Listed under
  [New or changed accepted risks](#new-or-changed-accepted-risks). The half of this finding that
  was about losing mail still holds, and holds better than it shipped:
  - **A full mailbox is "later", not a refusal** — the quota mapping this fix asked for, which 0.5.2
    did not implement (`fix(fetch): wait for room instead of losing mail to a full mailbox`). A
    `552 5.2.2` from storing a fetched message is now read by its enhanced status code X.2.2 and left
    at the provider until it fits; before, it was stepped past for good. Without this, the change
    above would have deleted mail at the provider whenever the mailbox here was full.
  - **Mail this server has nowhere to put** — the mailbox it fetches into is gone, or its address
    takes no mail — is `Taken::Nowhere`, not `Taken::Refused`, and is still left untouched.
  - The regression test above is replaced by two: `a_refused_message_is_cleared_at_the_provider`
    and `a_full_mailbox_here_never_costs_the_mail_at_the_provider`
    (`crates/uwumail-server/src/fetch.rs`), each checked to fail without the change it guards.

### S-9 · Medium · A fetched message answered "later" is marked seen on the first attempt and dropped on the next

`crates/uwumail-server/src/fetch.rs:310-323`

- **CVSS 3.1:** `AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:L/A:L` (Medium)
- **Attacker & preconditions:** no attacker required: any fetched message greylisted (`451`) or
  hitting a transient store error takes this path.
- **Impact:** `mark_fetch_seen` runs **before** `deliver_fetched` and returns "already here" on the
  next run, so the worker reports `Taken::Kept`, marks the message read or deletes it at the
  provider and moves on. With greylist-hold on it survives only in the waiting list for two days;
  with hold off, or for messages over 5 MB / beyond 200 held, or after a storage failure, it is
  lost — contradicting the documented "a message is never lost, the next run offers it again".
- **Evidence:** code read of `fetch.rs:310-323` and `crates/uwumail-store/src/fetch.rs:671-682`
  (`ON CONFLICT DO NOTHING` returns true when the key already existed; the seen row is not removed
  on `Taken::Later`).
- **Fix:** on `Taken::Later` remove the seen row (or record the UID so it is genuinely re-offered),
  and only mark seen after a successful delivery.
- **Regression test:** worker test: a message that is greylisted on the first run is delivered on
  the second, not dropped.

### S-10 · Medium · A user can aim the fetch/send worker at internal hosts, and the banner comes back in the error

`crates/uwumail-store/src/fetch.rs:303-309`, `crates/uwumail-smtp/src/outbound.rs:172-207`

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:C/C:L/I:N/A:N` (Medium)
- **Attacker & preconditions:** any logged-in portal user. `check_host` only requires a dot, so an
  IP literal or a name resolving to loopback/LAN/docker/gateway addresses is accepted for both the
  IMAP `host` and the send-as `smtp_host`; ports are any `u16`.
- **Impact:** blind-ish SSRF / internal port scan from the server's network position, deliberately
  bypassing the gateway because non-global targets connect directly. The exact error text
  distinguishes open/closed/filtered and returns the peer's first bytes — the IMAP path shows it
  immediately in the portal (`connection refused` vs `TLS … corrupt message` vs
  `the server did not greet: <first line>`), and the send-as path copies the SMTP greeting into the
  queue error and later verbatim into the DSN to the attacker's return path. No credentials leak
  (AUTH only after a certificate valid for the typed name), so this is reconnaissance, not access.
  The list fetcher already refuses non-public addresses; this feature does not.
- **Evidence:** code read of `check_host` (`fetch.rs:303-309`), `outbound.rs:172-207` (no address
  filter in `lookup`) and `crates/uwumail-server/src/gateway.rs:96-98` (non-global targets bypass
  the gateway). Not run live.
- **Fix:** resolve `host`/`smtp_host` and refuse non-global targets with the same `PublicResolver`
  rule the list fetcher uses, both when saving and at connect time; restrict ports to 993/143 and
  25/465/587; reduce the stored error to a short fixed category.
- **Regression test:** store test: create/update with `127.0.0.1`, `10.1.2.3`, `[::1]`, `localhost.`
  and a name resolving to a private address → `Invalid`. Worker test: no socket opened for a
  private target; the DSN carries a fixed reason, not the banner.

### S-11 · Medium · The SMTP "sending" switch is not consulted when mail is sent through JMAP or the webmail

`crates/uwumail-smtp/src/submission.rs:116-133`

- **CVSS 3.1:** `AV:N/AC:L/PR:L/UI:N/S:U/C:N/I:L/A:N` (Medium)
- **Attacker & preconditions:** any authenticated holder of an account whose SMTP switch an admin
  turned off (a spammer, a receive-only service, a read-only person), using the account password,
  an app password with the `mail` scope, or the webmail session cookie.
- **Impact:** outbound mail leaves the server (DKIM-signed, through the queue/relay/gateway) although
  "SMTP — sending through this server" is off for that account. `Smtp::submit` checks
  `account_owns_address` and the virus scanner but never `account.may_use("smtp")`; the JMAP path
  calls it directly. The switch documented as holding "every password at the door" only holds on
  587/465. A spamming account keeps sending from the webmail or any JMAP client. (This is the same
  class as 0.4.0 F-2, but a distinct gap — F-2 was the five-minute cache; this is the JMAP door
  never checking the switch at all.)
- **Evidence:** code read of `submission.rs:116-133` and
  `crates/uwumail-jmap/src/methods/submission.rs:405-413`; `grep` confirms `protocols.smtp` is read
  only by the admin/CLI setters.
- **Fix:** in `Smtp::submit` (or `create_one` before it) refuse when `!account.may_use("smtp")` —
  a `SubmitError::SendingOff` mapped to JMAP `forbiddenToSend` and SMTP `550 5.7.1`; document that
  the switch governs JMAP and the webmail too.
- **Regression test:** `cargo test -p uwumail-jmap`: an account with `smtp: false`, then
  `Email/set` + `EmailSubmission/set` → `notCreated forbiddenToSend`.

### S-12 · Medium · The host helper writes its result files through symlinks the container can plant

`deploy/host/helper:196-208`

- **CVSS 3.1:** `AV:L/AC:L/PR:H/UI:N/S:C/C:N/I:H/A:L` (Medium)
- **Attacker & preconditions:** a process already running as the container's user (uid 10001), e.g.
  after an RCE in the server. The container shares the bridge directory with root
  (`0770 root:10001`) and may create directory entries there, including symlinks.
- **Impact:** the host task service runs as **root** with no filesystem sandbox (by design). It
  writes `job-<id>.json(.tmp)`, `job-<id>.log` and `machine.json.tmp` by name into the shared
  directory with plain shell redirection, then `mv -f`/`chmod`. `>`/`>>`/`chmod` follow symlinks and
  `fs.protected_symlinks` does not apply (the directory is neither sticky nor world-writable), so a
  link with one of those names pointing at any host path lets the container have root
  truncate/overwrite/append to that file (sshd/docker config, `/opt/uwumail/.env`, compose files)
  and `chmod` it — an arbitrary-host-file integrity primitive from inside the container. This breaks
  the boundary the helper exists for ("a server somebody took over can ask for updates or a restart,
  and for nothing more").
- **Evidence:** code read of `helper:178-208`; the id is validated only for shape.
- **Fix:** never write root's output by a name the other side controls in a directory the other side
  can populate. Split the bridge into an inbox the container writes and an out directory only root
  writes and the container reads; in the writers, refuse any target that is a symlink or not a
  root-owned regular file, create with `O_NOFOLLOW`.
- **Regression test:** a shell test (under the shellcheck job) points the state dir at a temp
  directory, plants `job-<id>.json.tmp`/`.log` as symlinks to a canary and asserts the canary is
  untouched.

## Low findings

Server (`S-…`) unless marked gateway (`G-…`) or release (`R-…`). Each is real but bounded; several
are defence-in-depth. CVSS vectors are in the private notes file.

- **S-13 · Fetch accounts of trashed accounts keep running** — `crates/uwumail-store/src/fetch.rs:342-354`.
  `fetch_accounts_due` selects on `enabled` only, with no join on `accounts`, and `trash_account`
  does not disable fetch rows. A trashed person's provider password keeps being used every interval,
  pulling and (with delete) destroying their provider mail, while port 25 already refuses them.
  *Fix:* join `a.deleted_at IS NULL` in `fetch_accounts_due` and disable fetch rows in
  `trash_account`.
- **S-14 · `deliver_fetched` skips `delivery_target`/`has_mailbox`** *(regression)* —
  `crates/uwumail-smtp/src/inbound.rs:249-258`. Fetched mail is stored into an account whose mailbox
  is switched off instead of following its redirect or being refused, unlike the RCPT path.
  *Fix:* resolve `delivery_target(account_id)` in `deliver_fetched`; refuse fetch accounts for
  accounts without a mailbox.
- **S-15 · DSN "sender verified" accepts an unaligned DKIM pass** —
  `crates/uwumail-smtp/src/checks.rs:88`. A forged `MAIL FROM` still earns a bounce when the attacker
  signs with their own domain, because any passing signature counts. Narrow (needs a partial-failure
  delivery) but classic backscatter. *Fix:* require SPF pass for the `MAIL FROM` domain or a DKIM
  `d=` related to it; treat a missing verdict as not verified.
- **S-16 · Vacation auto-replies go to unverified senders, keyed on the raw address** —
  `crates/uwumail-smtp/src/vacation.rs:50-53`. A forged `MAIL FROM:<victim>` earns a DKIM-signed
  auto-reply (backscatter), repeatable by varying `victim+1@`, `victim+2@` because the once-per-sender
  record uses the raw string. *Fix:* reply only when the envelope sender is verified; key on the
  normalised base address.
- **S-17 · A spam-trap co-recipient turns rejects into Junk delivery and disables greylisting** —
  `crates/uwumail-smtp/src/inbound.rs:1303-1306`. Adding `RCPT TO:<trap>` beside real recipients
  makes a message that would be rejected for everyone be accepted into the real recipients' Junk,
  and a Suspicious message skip greylisting into their inbox. *Fix:* decide reject/greylist over
  non-trap recipients only, still learning from the trap copy.
- **S-18 · Forged `Authentication-Results` survive stripping** —
  `crates/uwumail-smtp/src/headers.rs:103-109`. A quoted or comment-prefixed `authserv-id`, or an
  `A-R` placed after a line `split` refuses, keeps the server's own authserv-id on it. Impact limited
  to downstream readers (the server itself takes no decision from `A-R`). *Fix:* skip CFWS, strip
  quotes/trailing dot before comparing, run the strip over the header set mail-parser sees.
- **S-19 · Outgoing DKIM does not oversign present headers** — `crates/uwumail-smtp/src/dkim.rs:21-36`.
  A replayed copy of a signed message with a new `Subject`/`To` prepended still verifies `dkim=pass`
  (and DMARC pass) at conformant verifiers, including UwUMail's own. *Fix:* list identity/display
  headers twice in `SIGNED_HEADERS` to get RFC 6376 §8.15 oversigning.
- **S-20 · X-Forwarded-For walk skips an unparseable `ip:port` hop** —
  `crates/uwumail-server/src/http.rs:102-113`. A trusted proxy that appends the client as `ip:port`
  (IIS/ARR, Azure Front Door) has its entry dropped by `filter_map(parse)`, so the client picks its
  own address and escapes the per-IP login throttle. The stock Caddy/nginx setups write bare
  addresses and are unaffected. *Fix:* stop the walk at the first non-bare-IP hop and strip a
  `:port` suffix before parsing.
- **S-21 · Login cache stamps its time after the slow password check** —
  `crates/uwumail-jmap/src/auth.rs:265`. A credential change during the argon2 verify is not seen for
  up to five minutes on JMAP/DAV. *Fix:* take the timestamp before the check.
- **S-22 · A cached login can map to a reused SQLite rowid after a purge** *(revised to Medium)* —
  `crates/uwumail-jmap/src/auth.rs:239`. If a purged account had the highest id and a new account is
  created within the five-minute cache window, the old login/password opens the new account. Narrow
  but high-impact if it lands. *Fix:* bind the cache entry to `created_at`/login, and set
  `credentials_changed_at = created_at` for new accounts.
- **S-23 · Admin takeover actions skip the password re-entry window** —
  `crates/uwumail-web/src/routes/people.rs:351`. Setting another account's password, resetting its
  second factors, minting a reset link and creating an admin all skip `confirm_identity`, which far
  less consequential actions (pairing a gateway, showing the recovery key) require. Only removes the
  last speed bump for a stolen live admin session (extends accepted risk A-3). *Fix:* call
  `confirm_identity` in those handlers.
- **S-24 · Import password / secret settings can be passed as CLI arguments** —
  `crates/uwumail-server/src/cli.rs:414-418`. `--password <value>` for `import imap`, and a literal
  `Secret` value for `settings set`, are visible in `ps`/`/proc/<pid>/cmdline` and shell history.
  *Fix:* make `--password` a value-less switch reading from stdin/file; refuse a literal for a
  `Secret` key.
- **S-25 · Provider-announced IMAP literal/message sizes are unbounded** *(revised to Medium —
  full in-process server crash + crash-loop, remotely triggerable)* —
  `crates/uwumail-server/src/import/imap.rs:89-163`. A
  hostile or user-chosen provider greeting such as `* OK {9223372036854775808}` makes `vec![0; size]`
  abort; with `panic=abort` the whole server dies and the fetch account retries into a crash loop.
  `deliver_fetched` also applies no `max_message_size`. *Fix:* cap literals and line length before
  allocating; bound fetched bodies to `max_message_size`.
- **S-26 · MTA-STS policy fetch leaves from the home IP despite the gateway** —
  `crates/uwumail-smtp/src/https.rs:36-38`, `crates/uwumail-smtp/src/mta_sts.rs:158-190`. The HTTPS
  policy fetch uses a plain `HttpConnector`, not the gateway connector, so any recipient domain
  publishing `_mta-sts` (or one that provokes a bounce/vacation reply) learns the home address —
  contradicting the gateway's "never your home address" promise. *Fix:* route the fetch through the
  gateway connector (add 443 to the gateway's outbound ports) or document the exception plainly.
- **S-27 · IMAP login limiter folds every IPv4-mapped peer into one key** —
  `crates/uwumail-imap/src/session.rs:420`, `crates/uwumail-smtp/src/limiter.rs:47-56`. The direct
  IMAPS listener feeds raw `::ffff:a.b.c.d` addresses to the limiter, whose /64 key collapses them
  all to `::`, so one IPv4 client's failures throttle IMAP login for all IPv4 clients, and the ban
  forwarded to the gateway names an address fail2ban cannot apply. DoS-class, reported for the
  keying defect and the mis-addressed ban. *Fix:* canonicalise the peer address like SMTP/HTTP do.
- **S-28 · The "next to a mail server" sample publishes plain-HTTP 8080 on all interfaces** —
  `deploy/next-to-mailserver/compose.yaml:38`. `${UWUMAIL_PROXY_BIND:-8080}:8080` defaults to
  `0.0.0.0:8080`, serving the whole site (login, JMAP, webmail) over cleartext with a non-Secure
  cookie and no HSTS, next to the intended proxy path. *Fix:* require the value or default to
  `127.0.0.1:8080`, and say so in `docs/deployment.md`. *(This matches the "port 8080 open" note on
  the test VM — the VM's own hand-built compose exposes 8080 the same way. It is a config default,
  not a code bug.)*
- **S-29 · Build/prep Docker stages use mutable tags, not digests** — `docker/Dockerfile.release:8`,
  `docker/Dockerfile.gateway:9`. The prep stage that `chmod`/`setcap`s the server binary and the
  stage that compiles the whole gateway binary use `debian:trixie-slim` / `rust:1-slim-bookworm` by
  tag; only the runtime base is digest-pinned, so "pinned by digest" covers the last layer only.
  *Fix:* pin every `FROM` by `@sha256`, moved deliberately with a CHANGELOG line.
- **G-1 · The gateway pairing token never expires** — `crates/uwumail-gateway/src/gateway.rs:493-518`.
  A generated-but-unconsumed code (from a log line, install output, a `.env` copy) stays valid until
  the legitimate server pairs, letting an attacker's server pair first and carry all public traffic.
  *Fix:* record when the token was minted and treat one older than a fixed lifetime as spent.
- **G-2 · The trusted (unbannable) address does not follow a QUIC path migration** —
  `crates/uwumail-gateway/src/gateway.rs:386,458`. `remote` is read once at connect and re-trusted
  every five minutes, so after a home-address change that quinn handles as a migration the stale
  address stays unbannable in every fail2ban jail (and a stranger later assigned it is spared),
  while the server's real new address is not protected. *Fix:* read
  `connection.remote_address()` on every report, or disable migration on the gateway endpoint.
- **G-3 · The root helper whitelists any CIDR found in the gateway-writable `trusted` file** —
  `deploy/gateway/hardening/helper:72`. A line `<addr> <now> 0.0.0.0/0` passes the shape check and is
  handed to `fail2ban-client addignoreip` for every jail, switching off SSH brute-force protection
  on the VPS. Strong precondition (code as the gateway user) but the helper is the side that is meant
  to re-check what the gateway writes. *Fix:* accept only `/32` and `/64`, or derive the range from
  the validated address.
- **G-4 · Gateway-authored text is presented to the admin as a root command** —
  `deploy/gateway/hardening/helper:549`. The MOTD and portal show `ssh root@<gateway> '<command>'`
  with a string the gateway chose (and raw terminal control sequences from `name`/`newRelease`),
  inviting the admin to run attacker-chosen text as root. *Fix:* compose the command from constants
  on both sides; drop `System.command` from the wire.
- **G-5 · The gateway helper follows symlinks writing job/machine files as root** *(revised to
  Medium — clean gateway-user → root escalation)* —
  `deploy/gateway/hardening/helper:246`. Same shape as S-12, in the gateway's own state directory:
  a local privilege-escalation primitive (arbitrary root file read via `chmod 0644`,
  truncate/append). *Fix:* keep root's outputs in a root-owned directory the gateway user only reads;
  refuse symlink targets.
- **R-1 · `cargo audit` does not gate the image push or the release** —
  `.github/workflows/ci.yml:168`. The `audit` job is in no `needs:` list, so a published RUSTSEC
  advisory turns it red while the same run still builds, pushes and releases. Trivy does not cover
  Rust crate advisories in the stripped binary, so this is the only advisory check in the chain and
  it is advisory-only. *Fix:* add `audit` to the `needs` of `image` and `gateway`; waive an
  unfixable advisory explicitly with `--ignore` and a CHANGELOG line.

## Informational

Design notes, hygiene, and corrections to earlier audit records — not bugs to rush, but written down
so they are not rediscovered as findings.

- **S-30 · JMAP submission is not bound by `max_recipients`/`max_message_size`** —
  `crates/uwumail-jmap/src/methods/submission.rs:391`. The SMTP door caps recipients (100) and size;
  the JMAP door does not. *Fix:* apply both limits in `Smtp::submit`.
- **S-31 · Relay security "none" sends AUTH PLAIN in cleartext even when the relay offers TLS** —
  `crates/uwumail-smtp/src/outbound.rs:366-368`. Selectable at runtime; the label says
  "only on your own network" but nothing enforces it, and unlike opportunistic MX delivery it does
  not even try STARTTLS. *Fix:* treat "none" as opportunistic, or refuse it with credentials for a
  non-private relay host.
- **S-32 · A null-sender SRS return is relayed even when scored Junk** —
  `crates/uwumail-smtp/src/inbound.rs:1295-1304`. For a valid SRS address the server acts as an
  anonymous remailer for 21 days and passes on Junk-scored mail; only the reject threshold, DMARC
  reject, sender-list reject and the virus scanner stop it. *Fix:* drop (still answering `250`) SRS
  returns whose verdict is Junk/quarantine.
- **S-33 · Traps train the server-wide Bayes filter without limit or review** —
  `crates/uwumail-smtp/src/inbound.rs:1451-1464`. An attacker who mails copies of the users'
  legitimate newsletters/invoices to a trap poisons the filter toward false positives; there is no
  per-trap cap, dedupe, or exclusion of DMARC-verified known-good mail. *Fix:* cap learning per trap
  per day, skip DMARC-passing known-good domains; document the risk.
- **S-34 · A bounce for a failed external forward discloses the forwarding target** —
  `crates/uwumail-smtp/src/dsn.rs:79-92`. The DSN to the original sender names the person's
  forwarding address. Standard MTA behaviour; listed so the owner can decide. *Fix:* report the
  forwarder's own address as Final-Recipient, or deliver the failure into the forwarder's mailbox.
- **S-35 · Fetch/import connects from the home IP, not through the gateway** —
  `crates/uwumail-server/src/import/imap.rs:125`. Every fetch run connects to the provider directly
  from home, so the provider's login log shows the home address the gateway is meant to hide (the
  send-as path does go through the gateway for public 465/587). *Fix:* document the exception, or add
  993 to the gateway ports and route the fetch through the connector.
- **S-36 · Operator-specific addresses in committed tests and history** —
  `crates/uwumail-smtp/src/fetch.rs:228`. The test list used the operator's test-VM LAN
  address as a "private" example and a real server address as a "public" one — contradicting the
  repository's own rule that no real hosts/internal addresses are committed. A private LAN address
  discloses nothing reachable; **nothing to rotate** (no keys/tokens/passwords were found anywhere in
  history — checked with gitleaks over the full history; the production hostname committed on
  2026-09-14 and removed on 2026-09-15 is public through DNS anyway). *Fix:* replace with
  documentation-range values.
- **S-37 · Audit-record correction: `no-new-privileges` is on ClamAV, not the server container** —
  `compose.yaml:47`. The regression record credits the `uwumail` service with a
  `security_opt: no-new-privileges` that only the `clamav` service carries. No escalation path today
  (distroless, no setuid, one file-capped binary). *Fix:* correct the record; optionally add NNP to
  the `uwumail` service and confirm it still binds 25/80/443.
- **S-38 · The workspace version on `main` lags the release, and the release job does not check it** —
  `Cargo.toml:17`, `.github/workflows/ci.yml:324-335`. `main` still says `0.5.0` while `0.5.1` was
  tagged on a separate branch; the release job only checks that a CHANGELOG section exists, never
  that `Cargo.toml` matches the tag, so a binary and image could disagree on the version the
  gateway's anti-downgrade check reasons about. *Fix:* bump the version before tagging **0.5.2**, and
  add a release-job check that `Cargo.toml` equals `${GITHUB_REF_NAME#v}`.
- **G-6 · Dual-stack household: only the tunnel's address family is spared from bans** —
  `crates/uwumail-gateway/src/gateway.rs:204`. A wrong password from a household device reaching the
  gateway over the other family can ban that address for an hour (TCP only). Availability, one
  device, self-inflicted. *Fix:* let the server register its other-family address, or document it.
- **G-7 · The home server announces all six tunnel services regardless of `listen.*`** —
  `crates/uwumail-server/src/gateway.rs:247`. A listener emptied at home stays public through the
  gateway; every service still applies its own TLS/AUTH gates, so no plaintext exposure follows by
  itself. *Fix:* derive the announced services from the `listen` config, or document that public
  ports are switched off only in `gateway.toml`.
- **G-8 · `install.sh` continues when the new gateway binary fails its own sanity check** —
  `deploy/gateway/install.sh:125`. `set -uo pipefail` (no `-e`) means a failing `--version` does not
  stop the script from replacing the working binary and restarting into failure. Availability,
  operator error in a manual run (the portal path has the helper's rollback). *Fix:* add `|| exit 1`
  after the `--version` and `check-config` guards.

## Regression check of earlier findings

The Server regression targets from the test brief (M1, M2, and the "held up" list) plus the accepted
risks from 0.4.0/0.5.0. Status: **holds** / **circumventable** / **no longer applicable**.

| Finding | What it protected | Status |
| --- | --- | --- |
| M1 · one `From`, each `From`/`Sender` against `claimed_addresses` (submission) | Sender spoofing on submission | **holds on submission, but circumventable in spirit via the fetch feature** — the ownership check still runs, but S-2 lets an account self-grant ownership of any address through an unverified fetch row. Independently, on the *receive* path S-3/S-4 (two `From` / parser differential) and S-5 (trusted-relay HELO) bypass DMARC/sender verification. |
| M2 · forwarding confirmation-spam throttle | Bounce/confirmation flooding via forwarding | **holds** — not reachable by any new finding; the throttle and empty-sender requirement are unchanged. |
| Portal session as a JMAP login (hashed token, `HttpOnly`, `SameSite=Strict`, `__Host-`, CSRF constant-time) | Session/CSRF integrity for the webmail | **mostly holds, one gap** — the cookie is still set correctly, but S-7 shows the *reader* accepts the un-prefixed name over HTTPS, undoing the `__Host-` guarantee against a planted cookie. |
| Serving attachments (`Content-Disposition: attachment`, `nosniff`, account+blob checked) | No attachment renders on the origin | **holds** — unchanged; the account/blob checks and headers are intact. |
| The sanitiser and the `srcdoc` frame (no scripts, own `default-src 'none'` policy) | Malicious mail HTML cannot run in the reader | **holds** — unchanged in the server; see the webmail report for reader-side notes (print document, paste). |
| SQL — every new statement binds its parameters | SQL injection | **holds** — the new fetch/trap/greylist statements bind parameters; no interpolation of values. |
| The container (distroless, digest-pinned base, unprivileged, read-only, caps dropped) | Blast radius of a server compromise | **holds, with one record correction (S-37) and one build-stage gap (S-29)** — the runtime is as described; `no-new-privileges` is only on ClamAV, and the build/prep stages are not digest-pinned. |
| The cross-repository (webmail) build pinned by full commit hash | A forked/renamed webmail repo cannot change a release | **holds** — the pin-and-verify is unchanged. |
| A-1/A-2/A-3 (0.4.0 accepted risks: service keeps its password; switches take effect next login; admin makes service app passwords without re-confirming) | — | **A-1/A-2 unchanged; A-3 widened by S-23** — the "no re-confirm" now also covers setting another account's password and resetting second factors, which is more than A-3 accepted. |

## New or changed accepted risks

- **The fetch feature connects to providers, and (send-as) to their SMTP, from configuration a
  normal user controls.** Even after the S-1/S-2/S-10 fixes, a user can still point the server at
  *public* provider hosts of their choosing and have it log in with stored credentials. That is the
  feature. What must not remain accepted is cross-account routing (S-1), self-granted ownership
  (S-2), or non-global targets (S-10).
- **CSS is still not filtered** (carried by the reader's scriptless frame) — unchanged from 0.5.0.
- **One-click unsubscribe (RFC 8058) is still not implemented** server-side — unchanged from 0.5.0;
  see the webmail report W-5 for a client-side gap in the mail path.
- **Traps accept and learn from any MX mail** (S-33) — this is the trap design; the accepted part is
  that traps take mail; the *unbounded, un-reviewed learning* should not stay accepted before real
  mail.
- **Refused fetched mail is cleared at the provider** (S-8, changed 21 September 2026 at the
  operator's request). With `after_fetch = delete`, a message the filter refuses is deleted at the
  provider, so a false positive — list mail a DMARC policy rejects, a virus scanner that is wrong —
  is gone for good; the spam history keeps sender, subject and reason for its retention period
  (30 days by default), not the message. An attacker who can make the filter refuse a message can
  therefore have it deleted there, which is no more than the filter's own verdict already decides
  here. Whoever wants a second look sets the mailbox to mark as read. What is *not* accepted: losing
  mail that was never judged — a full mailbox (now "later") and mail with nowhere to go
  (`Taken::Nowhere`) are both left at the provider.
- **The mail already in a fetched mailbox comes over unfiltered and unscanned** (added after 0.5.2,
  21 September 2026). Asked for by its owner, a fetched mailbox's existing mail is copied the way
  `uwumail-server import imap` copies one — filed where the provider had it, with its own date, past
  the spam filter *and* the virus scanner — because judging months-old mail against rotated DKIM
  keys refuses good mail, which the change above would then delete at the provider. The reach is
  the owner's own mailbox only (an account without one gets nothing), fed from a provider mailbox
  its owner has logged into, and duplicates are recognised by `Message-ID`. Accepted: an infected
  attachment that already sat in that provider mailbox arrives here unscanned, as it would through
  the migration import. Not accepted: this path taking mail for anybody but the fetch account's
  owner.

## Prioritised fix order

**Must fix before the server carries real mail / before 1.0:**

1. **S-1** (Critical) — scope outbound routing to the account. This is the one finding that lets one
   user read or send another user's mail.
2. **S-2** — refuse fetched addresses of hosted domains / local accounts and require proof of
   control before send-as. Fix together with S-1; they share the root.
3. **S-3, S-4, S-5** — the DMARC/sender-verification bypasses on the receive path. A mail server that
   displays `p=reject` senders it never checked is not safe to expose.
4. **S-10** — refuse non-global fetch/send targets (SSRF from the server's position).

**Fix in the 0.5.2 window (data-loss and auth correctness):**

5. **S-8, S-9, S-13, S-14** — the fetch data-loss cluster (refused/later/trashed mail destroyed or
   dropped at the provider).
6. **S-7** — transport-aware cookie reader.
7. **S-11** — enforce the SMTP switch on the JMAP/webmail door.
8. **S-6** — position-based trust of fetched `Authentication-Results`.
9. **S-12, G-5** — the root-helper symlink primitives (host and gateway).

**Fix as hygiene / defence-in-depth:** S-15 … S-29, G-1 … G-4, R-1, and the informational items.
**R-1** and **S-38** should be done as part of cutting the 0.5.2 release itself.

## What was not tested, and why

- **The production server.** Nothing was run against it; nothing about it was changed.
- **No active/invasive tests anywhere.** Per the test rules: no fuzzing, brute-force, DoS or
  outbound mail. So no finding was proven by delivering a malicious message end to end — the
  fetch-cluster findings are established from the SQL, the schema and the call sites, not from a
  live interception.
- **The test VM** was used only non-invasively: reading configuration, confirming which listeners
  answer, with throwaway accounts on the existing test domain that were removed afterwards. (Note:
  the VM's own hand-built compose exposes port 8080 in cleartext on all interfaces — the same
  default as S-28; that is VM hygiene, not the product.)
- **The parsers were not fuzzed** in this pass (the earlier CI fuzz harness still runs). S-25 is the
  one allocation-size defect found by reading; a fuzz run of the IMAP-response, SMTP DATA and MIME
  parsers is still worth doing.
- **The portal and webmail in a browser with a real login** — signing in means typing a password
  into a form, which this review does not do; the interfaces were read and exercised against their
  mock and over the API.
- **The dependencies themselves**, beyond what the CI audit steps check.

## What was actually run

- **Scanners over both repositories:** `gitleaks` over the full git history (the only hits are dev
  mock fixtures in the webmail — see its report — nothing to rotate), `semgrep` (Rust + TS; the four
  hits are `rejectUnauthorized: false` in `dev/smoke.mjs`, a dev-only local smoke test, not shipped),
  `trivy` and `hadolint` on the images, `actionlint` on the workflows (one intentional word-split in
  `ci.yml:46`, informational). `cargo audit`/`pnpm audit` clean at the time of the pass; `eslint`
  clean on the webmail.
- **A fresh `fmt` + build + the webmail route test** on the release worktree (green).
- **Non-invasive checks against the test VM:** which listeners answer, the container's hardening
  flags, and the account inventory — throwaway accounts created via the CLI on the existing test
  domain and removed afterwards (only the operator's own accounts remained).
- The fetch-cluster root cause (**S-1**) confirmed by reading the exact SQL in `fetch_sender`, the
  `UNIQUE (account_id, address)` schema, and the `sender_route` call site — corroborated by the
  independent verification voters that completed before the session limit, all of which upheld it.

## Addendum — the settings sync extension, 22 September 2026

A focused pass over the settings extension that landed after 0.6.2 (`urn:uwumail:jmap:settings`,
[docs/jmap-settings.md](jmap-settings.md)): the store (`crates/uwumail-store/src/user_settings.rs`,
migration `0031_user_settings.sql`), the JMAP methods and push (`crates/uwumail-jmap/src/methods/settings.rs`,
`push.rs`, `session.rs`), the portal preferences that the extension mirrors
(`crates/uwumail-store/src/web.rs`) and the portal's new webmail preferences (commit 8b5553c). It was
reviewed together with the webmail's side of the sync, whose findings continue in
[UwUMail-Webmail/docs/security-audit-2026-09.md](https://github.com/MinifyX/UwUMail-Webmail/blob/main/docs/security-audit-2026-09.md)
(W-12 to W-21).

Threat model: a **logged-in account** against the server and against other accounts, and the values
themselves as **untrusted input for every device** of the account, because what one device writes is
handed to all the others.

**What held up.** Every call checks `accountId` against the signed-in account, and the integration
tests cover reading and writing another account's settings (`accountNotFound`, nothing changed).
All SQL is parameterized. The request body is capped (`maxSizeRequest`) before it is parsed and
`serde_json` limits nesting, so no deep or huge document reaches the settings code. Keys are on a
strict whitelist with a rule per value; limits on keys, total size and value size hold, and a
refused write changes nothing. Push only ever reports changes of the listener's own account, and the
settings state is read for that account alone. The portal mirror validates in both directions
(portal preference list, settings whitelist), and a portal write moves the settings state so the
apps hear about it.

| ID | Severity | Finding | Status |
| --- | --- | --- | --- |
| S-39 | Low | One `UserSettings/set` could name an unbounded number of keys | fixed in eb3ef99 |
| S-40 | Informational | Keys and entries that are special in JavaScript or in text direction are accepted | accepted, clients handle it |
| S-41 | Informational | Signature HTML is stored as written | accepted by design |

- **S-39 · Low · One `UserSettings/set` could name an unbounded number of keys** —
  `crates/uwumail-jmap/src/methods/settings.rs` (`parse_patch`), `crates/uwumail-store/src/user_settings.rs`
  (`update_user_settings`). The limits counted only what would end up stored. Removals in a patch
  and `null`s in a whole replacement were not counted, and each costs a statement inside the one
  write transaction, so a single request up to the body cap could name a few hundred thousand keys
  and hold the database's only writer — delivery included — for all of them, again and again. Only
  a signed-in account can do it, and nothing is read or changed that isn't theirs. *Fix:* an update
  names at most `maxKeys` keys, removals included; checked in the method before any validation and
  again in the store. Tests in both crates; the limit is documented in `jmap-settings.md`.
- **S-40 · Informational · Keys and entries that are special in JavaScript or in text direction are
  accepted** — a signature id may be `__proto__` or `constructor` (letters and `_` are allowed), and
  list entries and signature names may contain Unicode format characters such as a right-to-left
  override (only control characters are refused). Neither harms the server; both are data a client
  has to treat as data. The webmail now checks for its own keys only (W-19) and isolates names it
  shows (W-21). The desktop app shares `settingsSync.ts` with the webmail and needs the W-19 change
  too.
- **S-41 · Informational · Signature HTML is stored as written** — by design and documented: the
  server does not know how a client will show it, so every client cleans it like mail HTML before
  showing or inserting it. The webmail does (`cleanSignatureHtml`: the composer's cleaner, pictures
  only as embedded raster `data:` URLs).

Not a security matter, noted in passing: the portal's list of swipe actions has no `spam`, so the
webmail keeps that choice in the browser only.

**What was run:** `cargo fmt --check`, `cargo clippy -D warnings` and the tests of `uwumail-store` and
`uwumail-jmap` (`--test-threads=2`), all green. Nothing was run against a live server.
