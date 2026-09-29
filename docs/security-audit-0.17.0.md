# Security audit — what 0.17.0 added

A pass over what is new in 0.17.0, after the whole-stack audit of
[security-audit-0.16.0.md](security-audit-0.16.0.md): profile and contact pictures with Libravatar
and the `Face:` header, one-click unsubscribe through the server, the additions to JMAP Calendars
(per-user properties, notifications, `/copy` and `/parse`, alerts the server rings, availability,
custom time zones, event versions), and the Low fixes of the 0.16.0 audit where they touch those.

Scope: `git diff` of `main` against `release/0.17.0` — `uwumail-jmap` (pictures, profile,
unsubscribe, calendars, availability, time zones), `uwumail-smtp` (avatars, Face in and out, DKIM,
reminders), `uwumail-store` (pictures, contact photo index, received Faces, calendar notifications,
versions, alerts, per-user properties), `uwumail-dav` (privacy, free-busy), `uwumail-web` (picture
routes) and `web/` (the picture card).

Done with Claude, not an independent firm: the areas were read in parallel, each finding traced
through the code and, from Medium up, given a regression test that fails before the fix. Not a
certificate. No exploit code or attack payloads are written down here; the analysis says what was
wrong and the diffs show how it was fixed. Nothing was run against a production server.

The rule is the same as for 0.16.0: everything **Medium and above is fixed**; Low findings are fixed
where the fix is small and safe, the rest listed.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 1 | 1 | 0 |
| High | 3 | 3 | 0 |
| Medium | 6 | 5 + 1 partly | 0 (CAL-5 partly, see there) |
| Low | 11 | 7 + 2 partly | 2 (CAL-10, CAL-13), and the rest of CAL-8 and CAL-11 |
| Informational | 10 | 2 | 8 |

The serious findings are of two kinds. **Panics on text from outside:** the server runs with
`panic = "abort"`, and two new readers cut strings at byte positions that need not be character
boundaries — the rules of a custom time zone, which anyone who sends an invitation writes, and the
`PHOTO` of a contact card. Both ended the server for everyone, the time zone one again after every
restart through the alert worker. **Trust that was wider than it looked:** a Face was kept whenever
DMARC passed, though DMARC says nothing about a header no signature covers; a stranger's invitation
brought alarms along that the server now rings by mail; and CalDAV and a text filter over earlier
event versions showed people a calendar is shared with what its owner keeps private.

Nothing found lets someone into another account's mail, and the new cross-account paths (contact
photos, pictures of people here, sender picture caches, unsubscribe in shared folders, calendar
notifications and per-user properties) are scoped by account.

## Critical and High

| ID | Severity | What was wrong | Fix |
| --- | --- | --- | --- |
| CAL-1 | Critical | A custom time zone rule that calcard could not read reaches the server as text. Its `BYDAY` was cut at a byte position; a character of several bytes there panicked while the event was read. Any invitation from outside is stored with its time zones, and every stored event is read by the alert worker, so the server ended within seconds and again after every restart. | Rules are ASCII only and cut with checked splits; see CAL-2 for the values. |
| CAL-2 | High | A `BYMONTHDAY` far outside a month overflowed the date arithmetic of a custom time zone (panic). Over JMAP only the names of a rule's parts were checked, so it came in there as well as from iCalendar. | Months 1–12, days ±1–31, known weekdays, at most the fifth of a month, the same over JMAP and from iCalendar; checked arithmetic. Zones read from iCalendar keep the JMAP limits (10 zones, 20 rules, 100 onsets). |
| CONTACT-1 | High | The contact photo index cut a card's `PHOTO` value at bytes 5 and 8 to look for `data:` and `https://`. A character of several bytes there panicked inside the write transaction of the card: anyone who may write a card (CardDAV, JMAP, import) ended the server, and the backfill after the upgrade would have done so at every start. | The schemes are compared by bytes. |
| CAL-3 | High | A new copy of an outside organizer's invitation kept the organizer's VALARMs. With the server ringing alarms itself, a stranger could make it put reminder mails "from postmaster" into someone's inbox: twenty alarms on a series repeating every minute gave one mail per alarm every 20 seconds, planned anew after each. Someone who may write into a shared calendar could do the same to its owner. | Invitations keep only the attendee's own alarms. Reminder mails: at most one per event in four minutes and 100 per account a day. |

## Medium

| ID | What was wrong | Fix |
| --- | --- | --- |
| FACE-1 | Our DKIM signatures did not name `Face`, and incoming Faces were kept when DMARC passed — on SPF alone, or on a signature leaving the Face out. A signed mail sent again with another Face put that picture next to the sender's address for everyone on the server. | Our signatures oversign `Face`. A Face is kept only under a DKIM signature aligned with the From domain whose `h=` lists `Face`, with one From address and one `Face:` header. |
| FACE-2 | Faces were stored and shown for this server's own addresses, so a Face kept from someone's mail while it was public still showed after they turned their picture off, and one planted through FACE-1 or FACE-3 stood in for a colleague. | No Face is kept from our own domains, and `pictureUrl` does not look at stored Faces for them; the owner's visibility decides alone. |
| FACE-3 | Fetched mailboxes are the user's own to fill, and the provider's SPF result is taken on trust there; with DMARC as the gate this let any user plant a Face for any address. | Fixed with FACE-1: only a signature this server checks itself counts. |
| CAL-4 | `CalendarEvent/queryChanges` with `expandRecurrences` matched the query's text conditions against earlier versions of events, which are kept for everyone who sees a calendar — without reducing private events or leaving out secret ones. A sharee could learn what a private event had said, one question at a time, and for 30 days after the calendar was no longer shared. | Only the time window counts for earlier versions (more removed ids are allowed, RFC 8620 5.6); secret events count only for their calendar's owner, otherwise `cannotCalculateChanges`. |
| CAL-5 | Privacy was enforced over JMAP only: CalDAV handed the person a calendar is shared with the whole of a private or confidential entry, and both protocols let someone with write rights delete it (CalDAV also store over it). | CalDAV gives others only the times of an entry with a `CLASS` other than `PUBLIC` and refuses their PUT and DELETE of it; JMAP refuses the delete. **Partly:** a confidential entry still shows its times over CalDAV, where a collection cannot leave an entry out of its listings; over JMAP it is not there. |
| CAL-6 | Availability copied every instance of a series, description included, before looking at the window, and every instance in the window kept a copy of the stored event: a long series of a large event (an invitation can bring one) took gigabytes in one `Principal/getAvailability` or CalDAV free-busy request. | One instance at a time, the stored event shared by its instances, and at most 8 MiB of events carried along per answer. |

## Low

- **FACE-4** — one sending domain could push every other Face out of the 20,000 kept. **Fixed:** at
  most 200 per sending domain.
- **UNSUB-1** — `Email/unsubscribe` checked DKIM (whole message hashed, DNS asked) before any
  limit. **Fixed:** 120 checks per login an hour.
- **AVATAR-1** — sender picture lookups waited for one of eight turns without end, so a few slow
  domains held up pictures for everyone. **Fixed:** at most five seconds' wait for a turn.
- **PIC-1** — Libravatar answered for disabled accounts. **Fixed.**
- **CAL-7** — notifications decided what is private by searching the text for `CLASS:PRIVATE`,
  missing parameters, tab folding and unknown classes. **Fixed** with CAL-5: read as iCalendar.
- **CAL-8** — notifications carried the owner's per-user properties (alerts, colour, keywords) and
  all participants even with `hideAttendees`. **Partly fixed:** per-user properties are left out;
  hidden attendees are open with CAL-10.
- **CAL-9** — secrecy was checked on the main event of an object only, not on its other single
  instances. **Fixed** in `/get`, expanded `/query`, `/copy` and others' updates and deletes;
  calcard currently gives such an object the privacy of any instance, so it was not reachable.
- **CAL-10** — `hideAttendees` is applied in `/get` only: attendee and text filters, `/copy` into
  one's own calendar, `getAvailability` with details and notifications still see all participants.
  **Open.**
- **CAL-11** — the alert worker has no time budget per event, and time zones read from iCalendar
  had no limits. **Partly fixed:** those zones keep the JMAP limits (CAL-2); a budget per plan is
  open.
- **CAL-12** — pruning event versions scanned all stored contents of all accounts inside the write
  transaction. **Fixed:** only the contents of the removed versions are checked.
- **CAL-13** — per-user properties of shared events (256 KiB each), event versions (up to 64 MB per
  account) and notifications are stored outside the quota. **Open;** each is bounded per account.

## Informational

- CAL-14 — ids of secret events reach sharees through `/changes` and `queryChanges`, and
  `alreadyExists` can name one.
- CAL-15 — `Principal/query` by `calendarAddress` follows catch-alls, forwards and groups, unlike
  availability.
- CAL-16 — `CalendarEvent/parse` reads a blob before its size check, and one answer may carry up
  to 500 blobs of 4 MiB.
- UNSUB-2 — the unsubscribe link need not belong to the From or the signing domain (RFC 8058 does
  not ask for it), so any signed mail can make the server POST the fixed body to a public https
  address, 30 times an hour per login. `is_public` does not know NAT64, 6to4 or IPv4-compatible
  IPv6 ranges (all egress, not new).
- FACE-5 — fixed with FACE-1: a Face needs exactly one From address.
- FACE-6 — the `Face:` header was copied before its size was checked; it is checked first now
  (with FACE-1).
- FACE-7 — incoming Face PNGs are kept as they came (at most 2 KB, served with `nosniff`, a
  sandbox CSP and as an attachment).
- PIC-2 — `/avatar/<hash>?d=https://…` redirects anywhere, as the Libravatar protocol asks.
- PIC-3 — with `local=1`, a reader learns whether anyone on the server looked up a Libravatar
  picture of an address in the past week (the cache is server-wide by design; the same holds for
  company logos).
- PIC-4 — decoding an upload of 8000 × 8000 may take several hundred MB for a moment; one decode
  runs at a time on the whole server.

## What held up

- **Contact photos:** looked up in the reader's own address books and those shared with them
  (`dav_shares`) only, kept in the same transaction as every card write and move, gone with the
  card; linked `https:` photos are fetched through the egress like remote pictures and cached per
  account, never server-wide. `pictureUrl` answers only for the caller's own account, person
  pictures come with `Cache-Control: private, no-cache` and an ETag that only ever goes back to the
  same account.
- **Masked addresses:** no profile picture, logo, Face or Libravatar lookup or hash; only the
  reader's own card gives one a picture. Forwarding addresses and groups without a public picture
  are not in the Libravatar table.
- **Libravatar both ways:** SRV only for domains that are not ours, targets checked as host names
  and reached through the egress with public-address checks, 1 MB limit, type by magic bytes (no
  SVG), never Gravatar or libravatar.org; the provider answers public pictures only, with a
  bounded per-network limiter.
- **Face out:** a `Face:` the client wrote is always removed; one is added only for a person (not a
  service, shared mailbox or group) with a public, allowed picture and the switch on, sending from
  their own address or alias.
- **Decoding:** format by magic bytes, 8000 × 8000 and a decoder memory limit, one decode at a
  time, everything written anew without metadata; incoming Faces at most 64 × 64 and 2 KB.
- **Portal and JMAP picture APIs:** own picture for the session's account, services, groups, domain
  logos and the switches for admins only (with audit entries); `ProfilePicture/set` takes only
  blobs the account uploaded or holds in its mail; `ProfilePicture` is not available in shared
  accounts.
- **One-click unsubscribe:** the DKIM check is done again at the click against DNS (a stored or
  forged `Authentication-Results` counts for nothing), one signature must cover both headers, each
  header must appear once; https only, no userinfo, the checked address is the one connected to,
  no redirects, 20 seconds, 16 KB of answer, no cookies or referrer; a sharee needs read and write
  rights on a folder holding the message; once per message in five minutes, 30 per login an hour.
- **Calendars:** per-user properties are stored per (object, account) and read with the caller's
  id; notifications are scoped by account, capped at 200 and 30 days; `/copy` copies only within
  the caller's account and refuses others' private events; `/parse` checks blob access and caps
  blobs, bytes and events; alert pushes carry only ids; reminder mails go to the account itself,
  never to a VALARM's attendees, with the subject written by mail-builder; availability counts
  only a person's own addresses, never secret events, within 400 days and the request's time.
- **The 0.16.0 Low fixes** that touch the above (push to https on port 443 only, free-busy only
  for people one may know of, control characters out of DAV, shared states that move only with
  what is shared) hold with the new code.
