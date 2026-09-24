# Security review — before 0.8.0, 23 September 2026

The eighth pass, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md),
[security-audit-0.3.0.md](security-audit-0.3.0.md),
[security-audit-0.4.0.md](security-audit-0.4.0.md),
[security-audit-0.5.0.md](security-audit-0.5.0.md),
[security-audit-0.5.2.md](security-audit-0.5.2.md) and
[security-audit-0.7.0.md](security-audit-0.7.0.md). It covers the whole server at 0.7.1 (commit
`bcd0858`) and, in a second step, the code merged since then for the planned **0.8.0**: the egress
for remote pictures with `/jmap/image` and `/jmap/picture`, and JMAP Contacts.

Done with Claude, not an independent firm. An honest sweep, not a certificate. The reviewers read the
code; the fixes come with tests that were run, but nothing was run against a live server.

Finding ids are those of the review notes: `T-` mail transport, `A-` mailbox access, `W-` the web
side, `C-` JMAP Contacts, `P-` remote and sender pictures. The earlier S-numbers are not continued.
Each review area has a section of its own with its own summary; the web side and the infrastructure
follow in theirs.

## Transport and mailbox access

Scope: SMTP in and out (`crates/uwumail-smtp`), the fetch worker and its IMAP client
(`crates/uwumail-server/src/fetch.rs`, `import/imap.rs`), mailbox discovery (`autoconfig.rs`),
IMAP, ManageSieve, CalDAV/CardDAV, JMAP and the store; since 0.7.1 also `egress.rs`,
`pictures.rs`, `crates/uwumail-jmap/src/remote.rs` and the JMAP Contacts methods.

### What held up

- **The fixes of the earlier rounds.** Outbound routing per account (S-1), the send-as checks (S-2),
  the two-From and cut-short header block refusals (S-3/S-4), reading the client from the relay's
  comment (S-5), the SSRF gate for fetch accounts (S-10), the SMTP switch on every door (S-11/S-30),
  DSN and vacation backscatter (S-15/S-16), forged Authentication-Results (S-18), DKIM oversigning
  (S-19), the literal cap of the fetch worker (S-25), the IPv4-mapped limiter key (S-27), and all of
  the 0.7.0 ManageSieve, Sieve and calendar limits were re-checked and hold.
- **Accounts stay apart.** Every JMAP method pins its `accountId`; every store query, blob id and DAV
  path is scoped by the signed-in account. The new AddressBook and ContactCard methods resolve every
  id, `addressBookIds`, move and `onSuccessSetIsDefault` within the account, and are switched off with
  CardDAV. No cross-account path was found.
- **Remote pictures.** The egress resolves every name itself, keeps only public addresses and
  connects to exactly the address it checked, on every redirect and through the proxy alike, so DNS
  rebinding and redirects into the local network reach nothing. Bodies are capped, the whole fetch
  including redirects has one timeout, nothing but a picture is passed on, and what is passed on
  comes with `nosniff`, `attachment`, a sandboxing CSP and `same-origin` resource policy. Both
  endpoints need the account's own login; the portal cookie is `SameSite=Strict`, so no other site can
  make a browser ask on its user's behalf. The admin's egress status and test are admin-only.
- **DAV XML, SQL, recurrence limits, ManageSieve literals.** As in 0.7.0.

### Summary

| Severity | Found | Fixed |
| --- | --- | --- |
| High | 1 | 1 |
| Medium | 9 | 9 |
| Low | 10 | 0 |
| Info | 4 | 0 |

Plus one correctness bug found while writing the T-1 test, fixed here.

### Findings

#### T-1 · High · A BDAT chunk was kept in memory whatever its size

`crates/uwumail-smtp/src/inbound.rs` (`Session::run`, the `State::Bdat` arm)

- **Attacker & preconditions:** anyone who can reach port 25, with one valid local recipient.
- **Impact:** DATA stops keeping bytes once a message passes `max_message_size`. BDAT (CHUNKING)
  checked the size only after a whole chunk had arrived, and a chunk may announce any size up to
  `usize::MAX`. One connection could make the server hold as much as it cared to send; across the
  connection slots that is enough for an out-of-memory kill of the whole server.
- **Fix:** a chunk is judged by the size it announces, before any of it is read. One that would take
  the message past the limit is read and thrown away, and the message ends with `552 5.3.4` as before.
- **Status:** fixed in `051592f`.
- **Regression test:** `crates/uwumail-smtp/tests/flow.rs` `bdat_chunks_are_held_to_the_size_limit`
  (one oversize chunk, chunks that only together pass the limit, and a two-chunk message that
  arrives); `inbound.rs` unit test `a_bdat_chunk_is_judged_before_it_is_read`.

#### Correctness · `BDAT 0 LAST` was answered only after the next packet (fixed)

Found while writing the T-1 test. An empty chunk, the way many clients end a chunked message, is
complete without another byte, but the session waited for one more read before taking it, so the
client waited for an answer until the idle timeout. Fixed in `7b90599`, covered by the same flow
test.

#### T-2 · Medium · A From naming addresses in several domains was not judged by DMARC

`crates/uwumail-smtp/src/checks.rs` (`verify`)

- **Attacker & preconditions:** a sender with a domain of their own whose SPF passes.
- **Impact:** RFC 5322 allows several mailboxes in one `From`, and DMARC exempts a From whose
  addresses lie in more than one domain (RFC 7489 section 6.6.1); `mail-auth` then returns no verdict
  at all. A domain that publishes `p=reject` could be named next to the sender's own address and was
  neither refused nor quarantined. The 0.5.2 S-3 fix covered two From headers, not one with two
  domains.
- **Fix:** such a message is refused before it is judged, like one with two From headers. Several
  authors from one domain are unaffected.
- **Status:** fixed in `8c8b5dc`.
- **Regression test:** `flow.rs` `a_from_with_addresses_in_several_domains_is_refused`.

#### T-3 · Medium · The client's HELO went unchecked into this server's own Received header

`crates/uwumail-smtp/src/inbound.rs` (`Origin::received_header`)

- **Attacker & preconditions:** a sender on port 25 of a server that another UwUMail lists in
  `smtp.trusted_relays` (the chained setup).
- **Impact:** the downstream server reads the original client from the first bracket inside the
  first parenthesis of the `from` clause (the 0.5.2 S-5 fix). A HELO may contain `(`, `)`, `[` and
  `]`, and it stood right before this server's `([address])` comment, so the sender chose the address
  the downstream server ran SPF, DMARC, sender lists and reputation against.
- **Fix:** the HELO is written only when it is a host name or an address literal, otherwise as
  `unknown` — the writing half S-5 had asked for.
- **Status:** fixed in `80345bc`.
- **Regression test:** `inbound.rs` unit test `a_helo_cannot_name_the_client_address_downstream`,
  which reads the written header back with `relay::original_client`.

#### A-1 · Medium · IMAP `LIST` patterns took exponential time

`crates/uwumail-imap/src/mailboxes.rs` (`matches`)

- **Attacker & preconditions:** any account with IMAP switched on.
- **Impact:** the wildcard matcher tried every split for every `*` and `%`. The 255-character limit
  still allowed a pattern that, against a long mailbox name of the account's own, never finished;
  `LIST` runs on an async worker thread, so a few such commands stalled IMAP, JMAP, the portal and
  delivery together.
- **Fix:** the path is walked once with the set of pattern positions reached so far — at most pattern
  length times path length. Same results for `*`, `%` and `INBOX` as before.
- **Status:** fixed in `f86fc8d`.
- **Regression test:** `mailboxes.rs` `patterns_match_as_before` (every pattern and path up to four
  characters against the old matcher) and `a_hostile_pattern_is_answered_at_once`;
  `crates/uwumail-imap/tests/imap.rs` `a_hostile_list_pattern_is_answered_in_time`.

#### W-3 · Medium · Mailbox discovery read an IMAP server's lines without a limit

`crates/uwumail-smtp/src/autoconfig.rs` (`imap_stream`, `imap_login`)

- **Attacker & preconditions:** a portal user who controls a domain and a server; the candidates
  come from that domain's SRV records, autoconfig file or name guesses.
- **Impact:** the greeting and the answers to `LOGIN` were read with `read_line`, which grows until a
  newline. For the 20 seconds a candidate may take, a server could fill the memory with one endless
  line, several candidates and requests at once.
- **Fix:** a line is read up to 8 KiB, and a `LOGIN` gets at most 64 lines before its tagged answer.
- **Status:** fixed in `92fb127`.
- **Regression test:** `autoconfig.rs` `an_endless_imap_answer_is_given_up_on`.

#### T-7 · Medium · The fetch worker read a provider's lines without a limit

`crates/uwumail-server/src/import/imap.rs` (`read_response`), used by the fetch worker and the
migration import. Found while fixing W-3.

- **Attacker & preconditions:** a user who sets up a fetch account on a server they control.
- **Impact:** the S-25 fix bounds each literal, but a response line was read with `read_until`, which
  grows until a newline, and a command took any number of answers. Within the two-minute read timeout
  a provider could fill the memory with one line, or keep sending answers for ever.
- **Fix:** a line is read up to 16 MiB (a `SEARCH` answer for a very large folder still fits), and
  what one command's answers add up to is capped at a full batch of the largest literals plus room for
  lines. Behind that cap a batch of 25 messages of up to 64 MiB each can still take a lot of memory at
  once; see T-8.
- **Status:** fixed in `a292890`.
- **Regression test:** `import/imap.rs` `endless_answers_are_cut_off`.

#### P-1 · Medium · Sender pictures kept an entry for every domain ever asked for

`crates/uwumail-smtp/src/pictures.rs` (`SenderPictures::get`)

- **Attacker & preconditions:** any account that may use the webmail or JMAP, asking
  `/jmap/picture` for addresses at ever new domains.
- **Impact:** the per-domain lock was removed from its map only after a completed lookup, not after
  an answer from the cache, and domains nothing answered for were remembered with no end. Both maps
  grew by one entry per domain for as long as the server ran — slowly, but without a bound.
- **Fix:** the lock is removed by the last one who used it, whatever the answer; unreachable marks
  older than their 30 minutes are dropped when a new one is written.
- **Status:** fixed in `cfa9612`.
- **Regression test:** `pictures.rs` `a_website_icon_is_fetched_once_and_then_remembered` (no lock left
  behind) and `unreachable_domains_are_forgotten_in_time`.

#### C-1 · Medium · Contact work had no time limit per request

`crates/uwumail-jmap/src/methods/contact_card.rs`

- **Attacker & preconditions:** any account with CardDAV switched on, on its own cards.
- **Impact:** the S-45 fix for calendars was not carried over. Each `ContactCard/query` got its own
  five seconds, 64 times per request, and `/get` and `/set` — which convert every card between vCard
  and JSContact, up to 1 MiB each — had no limit at all, on the blocking pool the store also needs.
- **Fix:** calendar events and contact cards share the request's fifteen seconds
  (`methods::request_deadline`), checked per card in `/get`, per create and update in `/set`, and as
  the upper bound of a query's own limit.
- **Status:** fixed in `d8327d8`.
- **Regression test:** none that waits out the clock; the checks are the calendar ones.

#### C-2 · Medium · Query filters and sorts were unbounded

`crates/uwumail-jmap/src/methods/contact_card.rs` (`query`), and the same shape in
`calendar_event.rs` since 0.7.0

- **Attacker & preconditions:** as C-1 (for calendars: CalDAV switched on).
- **Impact:** every condition is checked against every object, a text condition reads the object's
  whole text, and the clock is only looked at between objects. A request-sized `OR` of text
  conditions against one large card ran for hours; hundreds of thousands of sort comparators made a
  key each per hit.
- **Fix:** a filter may hold 100 operators and conditions, a sort as many comparators as there are
  sortable properties; more is `unsupportedFilter` / `unsupportedSort`. Applied to calendar events too.
- **Status:** fixed in `d8327d8`.
- **Regression test:** `crates/uwumail-jmap/tests/contacts.rs`
  `huge_filters_sorts_and_patches_are_turned_away`.

#### C-3 · Medium · Checking a patch for overlapping paths took the square of its size

`crates/uwumail-jmap/src/jscal.rs` (`overlapping_paths`), used by `ContactCard/set` and
`CalendarEvent/set`

- **Attacker & preconditions:** as C-1.
- **Impact:** every path was compared with every other, on an async worker thread and before the
  object was even looked up. A patch with a few hundred thousand keys, which fits into one request,
  held a worker for a very long time.
- **Fix:** the paths are sorted by their segments and only neighbours compared; same answers as before.
- **Status:** fixed in `e5b507f`.
- **Regression test:** `jscal.rs` `patches_follow_rfc_8620` (more shapes) and
  `a_huge_patch_is_checked_at_once`; the contacts test above updates a card with a 200 000-key patch.

### Low / Informational (not fixed)

- **T-4 · Low · A fetched message may be stored above `max_message_size`.** `deliver_fetched` checks
  hops but not the size, so a message between the limit and the 64 MiB literal cap lands in the fetch
  owner's own mailbox, quota still applying. The S-25 comment promising the size bound is aspirational.
- **T-5 · Low · Opportunistic MX TLS accepts any certificate.** Standard for MX delivery and confined
  to non-enforced MX; MTA-STS and relays verify. Recorded so it stays that way; no change.
- **T-6 · Info · MTA-STS policy fetches, the fetch worker and discovery leave from the home address
  even with a gateway.** Carried over from 0.5.2 S-26/S-35.
- **A-2 · Low · CalDAV/CardDAV `PUT` parses and expands on the async worker thread.** The JMAP path
  moved this to the blocking pool in S-45; the DAV path did not. Bounded per object (1 MiB, 3000
  occurrences).
- **A-3 · Low · `calendar-query` and `PROPFIND` with data load every resource body of a collection.**
  The time range is applied after loading; self-limited by the account's quota.
- **C-4 · Low · A JMAP client can write raw vCard lines through property names.** calcard escapes
  values but writes group, property and parameter names from the `vCard` and `convertedProperties`
  hints as they come, so a name with CR/LF adds lines to the stored card. Only the account's own
  cards, which CardDAV lets it write anyway; it does contradict docs/jmap-contacts.md's promise that
  JMAP cannot store what a phone would choke on. Fix: allow only `[A-Za-z0-9-]` in those names.
- **C-5 · Low · Whole-account reads for contacts.** `ContactCard/get` without ids loads every card
  before counting them, `/query` always loads every card, and DAV data does not count against the
  quota (the same as calendars).
- **P-2 · Low · The egress reaches public addresses on any port, and says why it failed.**
  `/jmap/image` may name any public address and port, the server's own public address and a router's
  WAN address included, and the error names the HTTP status it got. Blind GET only, nothing but a
  picture is passed on. Fix: allow ports 80 and 443 only, and one generic error.
- **T-8 · Low · The fetch worker still holds a whole batch in memory.** After T-7 a command's answers
  are bounded, but by design a batch of 25 messages of up to 64 MiB each. Fetching sizes first or one
  message at a time would bound it to one message.
- **P-3 · Low · The egress's 32 slots are shared by everyone and waited for without a limit.** One
  account can occupy them with slow picture servers; the others' pictures then wait. Only pictures
  are affected.
- **P-4 · Low · `is_public` misses a few special ranges.** NAT64 (`64:ff9b::/96`), 6to4
  (`2002::/16`), IPv4-compatible (`::/96`), site-local (`fec0::/10`) and `192.88.99.0/24` count as
  public. They reach nothing inside a usual network; `uwumail_tunnel::net::is_global` covers them and
  could become the one shared predicate.
- **P-5 · Info · The sender-picture cache is shared by all accounts.** By design (one fetch per
  domain and week tells a sender nothing about who reads what); a fast answer does show that someone
  on the server asked for that domain within the week.
- **C-6 · Info · Release builds abort on panic.** A stored vCard that made calcard panic would stop
  the server on every read of it. No such panic was found in the conversion code; fuzzing
  `from_vcard`/`to_vcard` is worth doing.
- **C-7 · Info · Push lists `AddressBook`/`ContactCard` changes whatever the CardDAV switch says,**
  as it does for calendars; only change counters leak, to the account itself.

### What was run

- **Tests, on Windows with one test thread:** the new and neighbouring tests of each fix —
  `uwumail-smtp` unit tests of `inbound`, `checks`, `autoconfig` and `pictures`; the BDAT, DMARC and
  trusted-relay tests of `flow.rs`; `uwumail-imap` `mailboxes` and all of `tests/imap.rs`;
  `uwumail-jmap` `jscal` and all of `tests/contacts.rs` and `tests/calendars.rs`; the `import`
  tests of `uwumail-server`. `cargo fmt --check` and `cargo clippy --all-targets -D warnings` for the
  four crates. The whole workspace runs in CI.
- **Not run:** anything against a live server; the memory of an actual BDAT or IMAP flood was not
  measured, the bounds follow from the code.

### Cleanups

- `properties_or` in `methods/email.rs`, a pass-through around `properties`, is gone (`9402783`); a
  comment typo in `blob.rs` is fixed with it.
- Not done: merging the three public-address predicates (`fetch::is_public`, the store's
  `is_public_ip`, `servercheck::is_private`) touches the SSRF gate and the callers want slightly
  different things (see P-4); merging `shorten` and the IMAP `quoted` helpers across crates was not
  worth a shared module.

## Web portal, backup, gateway and deployment

Scope: the portal's JSON API, sessions, login, second factors and passkeys
(`crates/uwumail-web`), the HTTP layer (`crates/uwumail-server/src/http.rs`), the backups
(`crates/uwumail-backup`, the restore paths in the server), certificates (`acme.rs`), the gateway
and the tunnel (`crates/uwumail-gateway`, `crates/uwumail-tunnel`, the server's side of both), the
root helpers and installers under `deploy/`, `docker/`, `install.sh`, `update.sh`, `scripts/` and
the CI workflow. Finding ids `W-` for the web side, `INF-` for backup, gateway and deployment.

### What held up

- **Access control in the portal.** Every admin route takes the `Admin` extractor, every account
  route `Session`, and every account-scoped store call the session's own account id. No path from
  one account into another's data was found.
- **Sessions and CSRF.** `__Host-` cookies over HTTPS, `HttpOnly`, `SameSite=Strict`, tokens stored
  hashed, a 192-bit CSRF token compared in constant time on every method but GET and HEAD, and the
  transport-aware cookie reader of 0.5.2 S-7, in the portal and in JMAP alike.
- **Passwords, TOTP, passkeys and links.** argon2 with a dummy hash for unknown logins, a one-step
  TOTP window with replay block, hashed recovery codes, exact WebAuthn origin and RP-ID checks, and
  256-bit single-use password and forwarding links.
- **The earlier rounds.** The X-Forwarded-For walk (S-20), HSTS only with a trusted certificate, the
  Cloudflare token used once and never stored, the restore path checks (`Component::Normal` only,
  ids checked), gateway token expiry (G-1), no path migration (G-2), `/32` and `/64` only for
  trusted ranges (G-3), commands composed from constants (G-4), the `--version` guard (G-8), the
  helpers' verb and version checks and no downgrade, pinning on both ends of the tunnel, and the
  CLI secret handling (S-24) were re-checked and hold.
- **Dependencies.** `cargo audit` against the advisory database of 23 September 2026: no advisories.

### Summary

| Severity | Found | Fixed |
| --- | --- | --- |
| High | 1 | 1 |
| Medium | 6 | 5 |
| Low | 8 | 0 |
| Info | 6 | 1 |

W-3, the sixth Medium of the web review, sits in `crates/uwumail-smtp/src/autoconfig.rs` and is
fixed with the transport findings above. Plus one correctness fix in the webmail's policy.

### Findings

#### INF-1 · High · A backup server could turn encryption off and choose what a restore takes

`crates/uwumail-backup/src/format.rs` (`Codec::new`, `Codec::decode`),
`crates/uwumail-backup/src/lib.rs` (`get`, `manifest`)

- **Attacker & preconditions:** whoever controls the SFTP backup target — the storage the
  encryption exists to distrust.
- **Impact:** `uwumail-backup.json` lies on the target unauthenticated, and a server with a recovery
  key followed it: `encrypted: false` gave an unkeyed codec, so the next scheduled backup uploaded
  the database, every mail and the data directory's keys in plain text, while the portal still
  showed encryption on. A keyed codec also accepted objects without the encryption flag, object ids
  were only recomputed in plain repositories, and a manifest was not tied to its name, so a restore
  from a hostile target could be handed a database of the target's choosing.
- **Fix:** whether a repository is encrypted is the server's decision: with a key, a config that
  says otherwise and any object without the encryption flag are refused. Every object's id is
  recomputed from its content (keyed with HMAC when encrypted), so an authentic object cannot stand
  in for another. New manifests carry their name inside the sealed content; manifests from before
  0.8.0 have none and are still read. The setup assistant and `backup restore` use a key that was
  given even when the target claims the backup is unencrypted. Existing encrypted repositories are
  unaffected: all their objects are flagged.
- **Status:** fixed in `cc3aaa6`.
- **Regression test:** `crates/uwumail-backup/tests/backup.rs`
  `a_backup_server_cannot_turn_encryption_off_or_swap_what_it_holds` (a rewritten config, a plain
  object, an authentic object moved to another id, a manifest under another snapshot's name);
  `format.rs` `a_key_is_never_given_up_for_what_the_backup_server_says`.
- **Not covered:** a hostile target can still withhold newer snapshots and offer an older, authentic
  one. An unencrypted repository has no protection against its target by design; docs/backups.md
  says so.

#### INF-2 · Medium · A compromised gateway VPS could get a trusted certificate for the server's name

`crates/uwumail-server/src/acme.rs`, `crates/uwumail-smtp/src/dnscheck.rs`,
`crates/uwumail-web/src/cloudflare.rs`, `docs/gateway.md`

- **Attacker & preconditions:** code running on the gateway VPS, as root or as the gateway user.
- **Impact:** with a gateway, the host name's A/AAAA records point to the VPS and it answers ports
  80 and 443, so it passes HTTP-01 validation at any public CA. With such a certificate it can end
  TLS on 443, 993, 465 and 587 and read passwords and mail on their way home. docs/gateway.md said
  the VPS could not read TLS.
- **Fix:** the DNS check now recommends a CAA record for the host name with RFC 8657
  `accounturi` bound to the server's own Let's Encrypt account and `validationmethods=http-01`, and
  reports one that lets other accounts or CAs issue (warning) or no longer lets the server renew
  (wrong). The Cloudflare automation writes it only when it is ticked. docs/gateway.md explains the
  record, what breaks when the account changes, CT monitoring, and corrects the two claims.
  Opt-in, because a record bound to the account stops renewals on a new server without the old
  data directory.
- **Status:** fixed in `530d6a2`.
- **Regression test:** `dnscheck.rs` `caa_binds_the_host_name_to_this_servers_account`;
  `cloudflare.rs` `a_caa_record_is_only_ever_written_when_asked_for`.
- **Not covered:** other names on the certificate that point to the gateway (`mta-sts.`, `imap.` …)
  are only mentioned in the documentation, not checked.

#### INF-3 · Medium · The root helpers still followed symlinks the unprivileged side planted

`deploy/gateway/hardening/helper`, `deploy/host/helper`, `deploy/gateway/install.sh`

- **Attacker & preconditions:** code running as the gateway user on the VPS, or as uid 10001 in the
  container on the host.
- **Impact:** 0.5.2 G-5/S-12 made the helpers create files safely, but they still worked by name
  afterwards in a directory the other side can write: `chmod` after a rename and after a job had
  run for minutes, `>>` appends to a job log, a plain redirect for the list of fail2ban ignores,
  `mv` onto a name that could be a symlink to a directory, and the installer's
  `written.sha256.tmp`. Root could be made to change the mode of, append to, or overwrite files
  elsewhere.
- **Fix:** files are created with their final mode (a umask instead of `chmod`) and renamed with
  `mv -T`, which replaces a symlink rather than following it; a job's log is opened once with
  noclobber and written through that descriptor; the ignore list and the installer's checksum list
  moved to `/var/lib/uwumail-gateway-helper`, which only root can write, and are taken over from
  their old place once. File names, contents and modes stay as they were.
- **Status:** fixed in `26a260d`.
- **Regression test:** `deploy/tests/helpers.sh`, run in CI: every write and job of both helpers,
  with the other side winning each race by putting a symlink to a canary file in place.
- **Not covered:** the `ProtectSystem=strict` sandbox for the tick and machine units was left out;
  it could not be tried on a real VPS in this round.

#### INF-4 · Medium · A backup server could crash the whole server at every scheduled backup

`crates/uwumail-backup/src/lib.rs` (`object_ids`, `prune`), `format.rs` (`object_path`),
`sftp.rs`, `storage.rs`

- **Attacker & preconditions:** whoever controls the backup target.
- **Impact:** names from the target's directory listing went into `&id[..2]` unchecked; a one-byte
  name panicked, and with `panic = "abort"` that stopped SMTP, IMAP, JMAP and the portal, at every
  daily run. Objects and manifests were read and inflated in full, so a large file or a deflate
  bomb could exhaust memory.
- **Fix:** listed names are checked (64 hex digits under their prefix; snapshot names as this
  server gives them) and anything else is left alone with a warning. Every read stops one byte
  past a limit that fits what it reads — 64 KiB for the config, 256 MiB for a manifest, the chunk
  size for database chunks, a file's recorded size, 1 GiB for a mail — and inflating stops at the
  same limit.
- **Status:** fixed in `d5746ce`.
- **Regression test:** `crates/uwumail-backup/tests/backup.rs`
  `a_backup_server_cannot_crash_a_backup_with_what_it_lists`; `format.rs`
  `objects_are_inflated_no_further_than_their_limit`, `only_ids_and_snapshot_names_reach_a_path`.

#### W-1 · Medium · Any successful login reset its network's failure count

`crates/uwumail-smtp/src/limiter.rs`, `crates/uwumail-web/src/routes/auth.rs`,
`crates/uwumail-web/src/login.rs`

- **Attacker & preconditions:** anyone with an account of their own on the server; for the second
  factor, someone who also knows the victim's password.
- **Impact:** `record_success` removed the whole network's failures, so logging into one's own
  account now and then allowed unlimited guesses at another account's password, including an
  admin's. The five tries at a second factor belonged to one pending login, and a new one needed
  only the password, so TOTP codes could be guessed without limit. The same limiter serves IMAP,
  SMTP and ManageSieve.
- **Fix:** a success takes back only the failures of the login that succeeded; the rest of the
  network's count stays until the window ends. Each login also has a count over all networks: after
  ten wrong passwords in fifteen minutes, its tries are spaced to one per thirty seconds — a delay,
  not a lockout, so knowing a login name is not enough to keep its owner out. Wrong second factors
  count per account over all pending logins: after ten, no second factor is looked at for fifteen
  minutes, and the account's owner gets a security notice. Another account's success lifts
  nothing. IMAP, SMTP and ManageSieve pass the login to the limiter as well.
- **Status:** fixed in `27f25da`.
- **Regression test:** `limiter.rs` `a_success_does_not_forgive_what_others_tried`,
  `guesses_at_one_login_from_many_networks_are_spaced_out`; `login.rs`
  `a_new_pending_login_does_not_bring_new_tries`; `crates/uwumail-web/tests/api.rs`
  `logging_into_ones_own_account_does_not_reset_the_guesses_at_another`.

#### W-2 · Medium · The HTTP listeners had no header or idle timeout and no connection limit

`crates/uwumail-server/src/http.rs` (`serve`, `serve_connection`), `gateway.rs`, `serve.rs`

- **Attacker & preconditions:** anyone who can reach port 80 or 443 (directly or through the
  gateway), before logging in.
- **Impact:** hyper was built without a timer, so its default header timeout did nothing, and the
  first read that tells HTTP/1 from HTTP/2 had no deadline at all. Every connection got a task of
  its own with no cap. Enough idle connections used up the process's memory or descriptors, which
  also stops SMTP and IMAP from accepting.
- **Fix:** a timer with a 20-second header timeout, which also closes an HTTP/1 connection that
  waits that long for its next request; the first bytes have to arrive within the same time;
  HTTP/2 connections are pinged and closed when a ping goes unanswered. All HTTP listeners and the
  gateway share a limit of 4096 connections, and one network (an IPv4 address, an IPv6 /64) may
  hold 128 of them; behind a reverse proxy only the total counts. The JMAP event stream keeps
  working: the limits count connections, not how long they last.
- **Status:** fixed in `ce08482`.
- **Regression test:** `http.rs` `connections_are_limited_in_all_and_per_network`,
  `a_connection_that_sends_nothing_is_closed` (silent from the start, half a request, and a
  complete one against a running listener).
- **Not covered:** an HTTP/2 connection that answers pings may stay idle; the per-network limit
  bounds it.

#### Correctness · The webmail's attachment previews were blocked by its CSP (fixed)

The previews of text, CSV, JSON, calendar and contact attachments read the file back from its
`blob:` URL with `fetch`, which `connect-src 'self'` refused, so they never showed on a real server.
The webmail's policy under `/mail` allows `blob:` in `connect-src` now; the portal's stays
`'self'`. Fixed in `6cdfe85`, test `assets.rs` `only_the_webmail_may_fetch_blob_urls`.

### Cleanups

- **INF-9 / 0.5.2 S-36:** the real public IPv4 address and the old test-VM LAN address in
  `crates/uwumail-gateway/src/machine.rs`, `web/src/features/setup/reach.test.ts` and
  `crates/uwumail-tunnel/src/net.rs` are documentation addresses now, and the S-36 entry of
  security-audit-0.5.2.md no longer quotes them (`b897cd8`).
- One job-id generator for the host bridge and the gateway instead of two identical ones (`993119e`).
- A leftover `let _ = settings;` in `start_restore` (`99b3b48`).
- The cookie that logs out is built from the cookie-name constants, like the one that logs in (`5dc2466`).
- `loki.rs` uses the portal's `unix_now`, the webmail access check no longer asks twice whether a
  webmail was built in, and the module doc lists every page the portal serves (`dbd9f24`).
- Not done: the redacting `Debug` for config structs that hold secrets (INF C-7), the single
  `Confirmation` struct and `Client` extractor of the web review, the shared network-key helper for
  JMAP: behaviour-preserving but not worth the churn in this round, or touching the other fixer's
  crates. The `System.command` pass-through (INF C-1) stays until no supported portal reads it.

### Low / Informational (not fixed)

- **W-4 · Low · X-Forwarded-For and X-Forwarded-Proto are read from the first header line only.**
  Behind a proxy that adds a second line instead of appending, the client picks its own address.
  Caddy, the documented proxy, merges.
- **W-5 · Low · Mailbox discovery lets any user test passwords at other providers without a limit.**
  No per-account budget and no activity entry for `POST /api/account/fetch/discover`.
- **W-6 · Low · The password re-entry misses the settings that send data elsewhere** (backup target,
  relay, antivirus address, Loki), extending the deferred 0.5.2 S-23.
- **W-7 · Low · An admin-set password leaves older reset and invitation links working** for up to
  seven days.
- **W-8 · Low · One user can fill the server-wide queue of pending Apple configuration profiles**, and
  the API still returns a download link for a profile it dropped.
- **INF-5 · Low · The gateway can present private client addresses**, which skips spam scoring for
  that connection; an extension of the accepted "made-up client addresses" risk.
- **INF-6 · Low · The portal's gateway update command and the docs unpack into fixed `/tmp` paths**
  before `sudo bash`; a local user on the VPS could pre-create them.
- **INF-7 · Low · `scripts/mailcow-export.sh` passes the mailcow database and Redis passwords on the
  `docker exec` command line.**
- **W-9 · Info · Any account's sorting trains the server-wide Bayes filter.** A conscious choice to
  make, like the accepted S-33.
- **W-10 · Info · The webmail and the admin portal share one origin.** Defence in depth only; the
  sandboxed frame and `script-src 'self'` hold.
- **W-11 · Info · Security headers are only on the portal's HTML page**, not on JSON, XML and profile
  responses.
- **INF-8 · Info · A compromised gateway can crowd the server's own lines out of the portal's log
  view.**
- **INF-10 · Info · The release job keeps its git credentials** (`persist-credentials` is not off),
  though nothing after the checkout needs them.

### What was run

- **Tests, on Windows:** the new regression tests above and the test files they live in —
  `cargo test` for `uwumail-backup` (unit and `tests/backup.rs`), the limiter, `login.rs`,
  `dnscheck.rs`, `cloudflare.rs`, `assets.rs`, `session.rs`, `tests/api.rs` and `tests/health.rs` of
  the portal, the `http.rs`
  tests of the server, `machine.rs` of the gateway and `net.rs` of the tunnel, with
  `--test-threads=1`. `cargo fmt --check` and `cargo clippy --workspace --all-targets -D warnings`
  over the whole workspace. In `web/`: `pnpm format:check`, `pnpm typecheck`, `pnpm lint` and
  `pnpm test`.
- **Left to CI (Linux):** the full `cargo test --workspace`, `shellcheck` and the new
  `deploy/tests/helpers.sh` — neither Docker nor a Linux shell with real symlinks was available on
  the machine the fixes were made on.

### What could not be tested

- **A real gateway VPS and a real host.** The helper changes are exercised by
  `deploy/tests/helpers.sh` against a temporary directory, not by systemd on a VPS.
- **A real CAA record at a CA.** The record's syntax and meaning follow RFC 8659 and RFC 8657 and
  Let's Encrypt's documentation; no certificate was ordered against one.
- **An SFTP target that lies.** The backup tests use a local directory standing in for the target.
