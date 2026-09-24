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
