# Security audit — 0.36.0, shipments and built-in calendars

A pass over what 0.36.0 adds: shipments found in mail and kept per account (`uwumail-labels`:
`shipment.rs`, `detect.rs`, `mail.rs`; `uwumail-store`: `shipments.rs`, migration 0092;
`uwumail-smtp/src/labels.rs`; the AI fallback and the rescan in `uwumail-assist/src/shipments.rs`;
`Shipment/*` in `uwumail-jmap`), the built-in holiday and week-number calendars
(`uwumail-store/src/holidays.rs`, migration 0091, the read-only checks in `dav.rs` and
`birthdays.rs`, the `urn:uwumail:dav builtin` property in `uwumail-dav`, the account move in
`uwumail-server/src/moves.rs`, `routes/calendars.rs` and its catalog), and `zip` 8 → 9 in the
attachment check.

Done with Claude, not an independent firm; an honest sweep, not a certificate. It was a reading of
the code, not a test against a running machine. Step-by-step exploits are left out.

## Summary

| Severity | New | Fixed | Accepted / open |
| --- | --- | --- | --- |
| Medium | 1 | 1 | 0 |
| Low | 9 | 8 | 1 |

## Threat model

First: **a stranger who can send mail to an account.** Every byte of a mail is theirs: headers,
HTML, links, the numbers in the text, and what the model reads from it. They must not crash or
stall delivery, and what the server learns from their mail must not become a link, a status or a
name that people trust because the server shows it. Second: **someone with a share or an app
password** on an account. They must not see or change more than their rights allow. Third: **the
server an account moves from**, which the person chose but which is not necessarily UwUMail.

## Findings

### S-1 — A look-alike domain becomes the tracking link (medium, fixed)

- Place: `crates/uwumail-labels/src/shipment.rs:86–106` (`Carrier::of_host`), `:833`
  (`carrier_link`), `:326–333` (choice of `tracking_url`), `:315` (penalty for unauthenticated
  mail); `crates/uwumail-store/src/shipments.rs:488` and `:506`.
- Cause: `of_host` takes the label before the top-level domain and accepts any TLD
  (`dhl.<any>`, `amazon.<any>`, `hermes.<any>`, where `hermes.com` is not the carrier) and,
  before any two-letter TLD, any `co|com|org|net`. A link on such a host counts as the carrier's
  with confidence 0.95; unauthenticated mail only loses 0.1, so it stays above the 0.8 threshold.
  The link is then stored verbatim as `trackingUrl` (https only) and, by `coalesce`, the first
  mail's link stays. The last fallback (`:333`) also takes any number-less carrier link, even one
  of another carrier. Apps and Home Assistant show that link as the "track" button, so a phishing
  link gets the server's word.
- Fix: an exact list of registrable domains per carrier (and their real country domains), compared
  on the whole host or a dot-bounded suffix. When a number is known, store
  `carrier.tracking_url(number)` instead of the mail's link; keep the verbatim link only for a
  carrier without a public URL scheme, and only from authenticated mail.
- Fixed: `of_host` matches the whole host or a dot-bounded suffix against a fixed list of
  each carrier's registrable domains. With a known number `trackingUrl` is always the carrier's own
  page; a number-less link is kept only when it is on that same carrier's domain.

### S-2 — A share sees events and mail ids from mailboxes it may not read (low, fixed)

- Place: `crates/uwumail-jmap/src/methods/shipment.rs:45` (`allowed`) and `:122–165` (`to_json`).
- Cause: a shipment is visible to a shared view when any of its mails is in a mailbox it may read,
  and is then shown whole: every event, whose `text` is the subject of its mail, and every
  `emailId`, including those of mails in mailboxes the view may not read.
- Fix: in a shared view, keep only the events (and `emailIds`) whose mail lies in a mailbox the
  view may read, and the app-added events.
- Fixed: `allowed` keeps, for a shared view, only the events whose mail lies in a readable
  mailbox and the app-added ones; `emailIds` follow from the events.

### S-3 — An event's time is not bounded (low, fixed)

- Place: `crates/uwumail-jmap/src/methods/shipment.rs:322` (`events`);
  `crates/uwumail-store/src/shipments.rs:316` (`add_event`) and `:762–789` (`prune_shipments`).
- Cause: `addEvents[].at` takes any date. Status only moves forward from `status_at`, so one event
  far in the future freezes the shipment's status against every later mail, and pruning by the
  newest event never removes it.
- Fix: refuse (or clamp to now) an `at` more than a few minutes ahead, and one before, say, a year
  ago.
- Fixed: `addEvents` refuses an `at` more than a year ahead (`invalidProperties`); the store
  counts any event ahead of now as now for `status_at`, `delivered_at` and pruning. Old dates are
  harmless (they never move the status) and stay allowed for carrier history.

### S-4 — Spam can fill the 2000 shipments and real parcels are dropped (low, fixed)

- Place: `crates/uwumail-store/src/shipments.rs:23` (`MAX_PER_ACCOUNT`), `:426` and `:444`
  (`record_mail`).
- Cause: one mail can add up to 10 shipments, so a couple of hundred non-junk mails reach the cap.
  After that, new parcels from mail are skipped without a word until pruning (60 days) frees room.
- Fix: count only shipments that are not stale, or, at the cap, drop the oldest shipment found in
  unauthenticated mail instead of the new one; a separate, lower cap for unauthenticated mail also
  works.
- Fixed: at the cap, the oldest shipment that is done or had no news for 7 days is removed for
  the new one; only an account full of live parcels skips it.

### S-5 — A forged mail can move a known parcel's status (low, fixed)

- Place: `crates/uwumail-store/src/shipments.rs:404–412` (rule 1 in `record_mail`).
- Cause: a mail that names a known number, of any carrier when it names none, is taken as news
  about that shipment, authenticated or not. Whoever knows a number (it travels in shop mails and
  on labels) can mark it delivered or returned.
- Fix: let unauthenticated mail fill in only what is missing, not move the status; or only move it
  from mail whose sender matches the carrier or the shop of the first mail.
- Fixed: the SMTP path (`SenderTrust::vouches_for`), the rescan (`Authentication-Results` of
  our own hop) and the model path pass whether the mail is authenticated; unauthenticated mail can
  add events to a known shipment but not end it (delivered, returned, cancelled).

### S-6 — The shop name from the model is not checked against the mail (low, fixed)

- Place: `crates/uwumail-assist/src/shipments.rs:185` (`from_model`).
- Cause: numbers and `orderRef` must appear word for word in the mail; `merchant` may be any text
  up to 100 bytes. A mail written to steer the model can name a shop it did not come from, shown
  next to a real-looking parcel.
- Fix: keep `merchant` only when it appears in the From name, the From domain or the text of the
  mail (case-folded), else drop it.
- Fixed: `merchant` from the model is kept only when it appears, case-folded, in the From
  name, the From address, the subject or the text.

### S-7 — A move trusts `builtin` from any source server (low, fixed)

- Place: `crates/uwumail-server/src/moves.rs:406–415` (`pull_dav`);
  `crates/uwumail-dav/src/client/discover.rs:154`.
- Cause: a calendar whose PROPFIND answer carries `urn:uwumail:dav builtin` is not copied; instead
  the holiday or week-number setting is turned on. Many CalDAV servers store any dead property a
  client sets, and a calendar shared to the person on the old server can carry it from its owner.
  The calendar's own events are then left behind without an error.
- Fix: honour `builtin` only when the source answered as UwUMail (for example its own server
  header or a capability it alone sends) and the collection is read-only there; otherwise copy the
  events as for any calendar.
- Fixed: the PROPFIND asks for `urn:uwumail:dav server`, which only UwUMail answers on the
  calendar home; `builtin` counts only when the home answered it, else the events are copied.

### S-8 — The URL parser differs from the browser's (low, fixed)

- Place: `crates/uwumail-labels/src/shipment.rs:895–906` (`Url::split`), with `links_in` in
  `crates/uwumail-labels/src/mail.rs`.
- Cause: the host is cut out by hand, and entities in HTML links other than `&amp;` are not
  decoded, so the server and a client can read a different host from the same string. Bounded by
  the https check and `links_in` cutting at backslashes and quotes.
- Fix: accept a host only when it is plain `[a-z0-9.-]`; with S-1's rebuilt links the stored URL
  no longer depends on the parse.
- Fixed: `of_host` accepts only plain `[a-z0-9.-]` hosts, `links_in` decodes HTML entities
  before it cuts the link, and with S-1 the stored URL is rebuilt from the number.

### S-9 — The subject stays when the mail is deleted (low, fixed)

- Place: `crates/uwumail-store/src/migrations/0092_shipments.sql` (`email_id … ON DELETE SET
  NULL`), the event `text` written from the subject.
- Cause: deleting a mail keeps its shipment and the events with its subject, for up to 60 days.
  Someone who deletes a mail to be rid of it still finds the subject in apps and Home Assistant.
- Fix: when an event's mail goes, clear its `text` (keep status and time); or say so in the
  privacy text and the settings.
- Fixed: migration 0093 adds a trigger that clears the `text` of a mail's events when the mail
  is deleted; status and time stay.

### S-10 — A rescan can be started again and again (low, accepted)

- Place: `crates/uwumail-jmap/src/methods/shipment.rs:474–492` (`rescan`).
- Cause: each call starts the account's rescan from the beginning.
- Why accepted: owner only (not in the shared methods), one rescan row per account, 90 days at
  most, 50 mails a round with a pause, rules only and never the model. At worst the owner keeps
  their own server's background job busy.

## What held up

- No parser panics found: the byte slices in `own_number` and `amazon_order` come after length and
  ASCII checks; `window`, `parse_time`, `parse_date` and `percent_decode` fail to `None`;
  `order_number` stops after five matches. `links_in` reads at most 4 MB of markup, 100 links of
  2000 characters, in one pass.
- The server never opens a link: redirect wrappers are unwrapped from the URL alone, three deep at
  most. Shipments are only looked for in mail that is not junk and is kept, with a time limit and
  off the async threads.
- `Shipment/set`: length limits for every text, control characters refused, 200 events per
  shipment, 2000 per account; `trackingUrl` is server-set only. A shipment the caller may not read
  is `notFound`, one they may only read is `forbidden`; create needs write on the Inbox.
  `clientName` comes from the app password or OAuth grant, not from the request. Query filters and
  sorts are size-checked.
- The AI fallback runs only through `prepare(account, "autoLabels")`, so with the person's
  permission and inside the cost limits; the mail is fenced and escaped in the prompt, and every
  number must appear in the mail word for word.
- Turning shipments off deletes all of them and the rescan in the settings transaction; the
  rescan skips junk and trash. Migrations 0091 and 0092 cascade on the account and the collection.
- Built-in calendars: the `holidays` setting is checked against the fixed lists of countries and
  regions, `weekNumbers` must be a boolean, and the same check runs on what a move brings. Their
  entries refuse PUT, DELETE, MOVE and import (`check_entries_writable`), the collection refuses
  deletion through CalDAV and JMAP (`check_deletable`), JMAP shows them read-only and refuses
  `uwuBuiltin` on create. The `builtin` property is shown to the owner only and is not writable.
- The catalog route needs a session, is built from constants and fetches nothing; its list of
  feeds is empty. `zip` 9 skips file names it cannot decode instead of failing.
