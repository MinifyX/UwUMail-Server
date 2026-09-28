# JMAP Calendars

UwUMail Server serves the calendars people already have over CalDAV as JMAP
Calendars ([draft-ietf-jmap-calendars](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/),
in the RFC editor queue). The webmail and the UwUMail apps use it; phones and
Thunderbird keep using CalDAV, and both see the same calendars and events.

This page says what is supported and how the server behaves where the draft
leaves room. Sharing and invitations are explained for everyone in
[calendars.md](calendars.md).

## Capability

`urn:ietf:params:jmap:calendars` is in the session's `capabilities` (an empty
object), in `primaryAccounts` and in the account's `accountCapabilities`:

```json
"urn:ietf:params:jmap:calendars": {
  "maxCalendarsPerEvent": 1,
  "minDateTime": "1900-01-01T00:00:00Z",
  "maxDateTime": "2200-01-01T00:00:00Z",
  "maxExpandedQueryDuration": "P400D",
  "maxParticipantsPerEvent": null,
  "mayCreateCalendar": true
}
```

With it come `urn:ietf:params:jmap:principals:availability`
(`{ "maxAvailabilityDuration": "P400D" }` in the account) for
[`Principal/getAvailability`](#availability), and
`urn:ietf:params:jmap:calendars:parse` (`{}`) for
[`CalendarEvent/parse`](#calendareventparse).

Only accounts that may use calendars get them: when an admin switches CalDAV off
for an account (services have it off from the start), the capability is gone
from its session and every calendar method answers
`accountNotSupportedByMethod`.

The login has to be allowed calendars too. An app password or an OAuth app
limited to `mail` reads and sends mail, but calendars need the `dav` use, over
JMAP just as over CalDAV: for such a login the capability is left out of the
session, and calendar methods answer `forbidden`. The account password, the
webmail and tokens from `/jmap/token` ([jmap-tokens.md](jmap-tokens.md)) may
use them.

## One store for CalDAV and JMAP

There is no second copy. A Calendar is a CalDAV calendar collection, a
CalendarEvent is an event (`VEVENT`) stored in one; tasks (`VTODO`) stay
CalDAV's and are not shown, and neither are lists that only hold tasks, like
the reminders of Apple's devices. Events stay iCalendar on disk and are
turned into JSCalendar when read and back into iCalendar when written, with the
[calcard](https://crates.io/crates/calcard) crate, so JSCalendar property names
are the ones calcard uses (`recurrenceRule` in the singular, `showWithoutTime`,
`locations`, `recurrenceOverrides`, `excluded`, …).

- A change over CalDAV (PUT, DELETE, MKCALENDAR, PROPPATCH, deleting a
  calendar) shows up in `Calendar/changes`, `CalendarEvent/changes` and push.
- A change over JMAP moves the CalDAV sync token, CTag and the event's ETag,
  so phones pick it up with their next sync.
- What JMAP writes goes through the same check as a CalDAV PUT (one iCalendar
  object, one UID, a component the calendar allows, the CalDAV size limit of
  1 MiB) before it is stored, so a phone can always read it and store it back.
- Changed instances of a series are written out whole, as iCalendar has them:
  CalDAV clients find title, time and place in every override. Over JMAP an
  override only shows what differs from the series.
- The first calendar is made the first time either side looks, with the same
  name ("Kalender" or "Calendar", after the server's language) and colour.

## Shared calendars

Calendars other people of the server share with the account are listed by
`Calendar/get` next to its own, in the same account, with their own ids
(`c12` is the same calendar for its owner and everyone it is shared with).
Their events are in `CalendarEvent/get`, `/query` and `/changes`, and a change
to them is pushed to everyone who sees the calendar. What the account may do is
in `myRights`; writing where it may not is `forbidden`.

| Shared with rights | `myRights` |
| --- | --- |
| read | `mayReadFreeBusy`, `mayReadItems`, `mayUpdatePrivate` |
| read and write | also `mayWriteAll`, `mayWriteOwn`, `mayRSVP` |
| all | also `mayShare`: the description and `shareWith` may change |

`mayDelete` of a shared calendar is `true`: destroying it only leaves it, it
stays its owner's. `isDefault` is always `false` for it. A calendar shared with the
account has an extra property naming its owner:

```json
"uwuSharedBy": { "email": "mini@example.org", "name": "Mini", "principalId": "p3" }
```

(`null` for one's own calendars).

`shareWith` of an own calendar (or one shared with all rights) is a map from
principal id to CalendarRights, `null` when it is shared with nobody.
Principals are the people of the server, the same ones `Principal/get` and
`Principal/query` give (`urn:ietf:params:jmap:principals`, see
[sharing.md](sharing.md#principals)): a principal id is `p` and the account
number, like `p12`. As long as a client has no Principal list at hand, an
address of the server may stand for the principal id when writing
(`{ "leni@example.org": { "mayReadItems": true } }`), and so may the account id
(`a12`) that clients of UwUMail 0.11 sent; answers always use the principal id.
Set the whole map, or one person with `shareWith/p12` (`null` takes them off). The rights asked for
are rounded up to the three levels above: `mayShare` means all, any writing
right means read and write, any other right means read. Someone who is not on
the server is `invalidProperties` naming `shareWith`.

### Per-user properties

Everyone keeps their own settings of a calendar shared with them, as the draft
says: `name`, `color`, `sortOrder`, `isVisible`, `timeZone`,
`includeInAvailability` and the default alerts. Name, colour and time zone
start as the owner's, the others at their defaults (`isVisible: true`,
`sortOrder` 0, `includeInAvailability: "none"`). Changing them needs no rights
beyond reading and never touches the owner's calendar: the owner's CalDAV
clients and sync token see nothing of it. The CalDAV clients of the person it
is shared with show their own name, colour, order and time zone too, and a
`PROPPATCH` of these properties from them is kept as theirs.

The per-user properties of events (`keywords`, `color`, `freeBusyStatus`,
`useDefaultAlerts`, `alerts`, also in `recurrenceOverrides`) work the same
way: in a calendar shared with the account they start empty, and what it sets
is kept apart for it, whatever its rights; `updated` then shows the later of
the owner's change and its own. The owner's per-user properties are part of
the event and stay there, and only the account that changed its own hears
about it. CalDAV clients of the person it is shared with see the event as the
owner keeps it.

An event the owner marks `"privacy": "private"` shows others only its times
and the like (RFC 8984, section 4.4.3), is not found by their text searches
and cannot be changed by them, not even their own properties of it. A
`"secret"` one is not there for them at all.

## Availability

Every principal that is a person has a `urn:ietf:params:jmap:calendars`
capability: its `calendarAddress` (`mailto:` and its login),
`mayGetAvailability` (whether it uses calendars), `mayShareWith` and
`accountId` (the caller's own account for the caller, else `null`, as the
calendars shared with the caller are in its own account). `Principal/query`
also takes `calendarAddress`, which finds a person by any of their
addresses, never by a masked one.

`Principal/getAvailability` answers when a person of the server is busy
between `utcStart` and `utcEnd` (at most `P400D`, `tooLarge` otherwise).
Everyone who uses calendars may ask about the people who do in their own
domains and those who share a calendar with them, as with CalDAV's free-busy
lookups (`mayGetAvailability` says whom); anyone else is `forbidden`. What counts is what the draft says, from the
person's point of view:

- the calendars whose `includeInAvailability` is `"all"` or `"attending"`
  for them: by default their own ones, not subscribed ones and not those
  shared with them;
- events that are not `secret`, not `cancelled` and whose `freeBusyStatus`
  is `busy` (their own one for an event of a calendar shared with them), in
  an `"attending"` calendar only those they accepted or may attend;
- every instance of a series in the window.

`busyStatus` is `tentative` for tentative events and answers, else
`confirmed`. With `showDetails`, an event comes along (as `event`, cut to
`eventProperties`, with `accountId` the caller's) when it is in a calendar the
caller may read and is not `private`; all other periods are merged as the
draft asks. A lookup that runs out of the request's time answers `rateLimit`.

## Ids

| Object | Id | |
| --- | --- | --- |
| Calendar | `c12` | |
| CalendarEvent | `v34` | a stored event, a series included |
| An instance of a series | `v34_20261027T090000` | the event id and the instance's `recurrenceId` without `-` and `:`; only from `CalendarEvent/query` with `expandRecurrences` |
| Another single instance | `v34_20261103T090000` | the same, for the further instances of an object that holds single instances without their series (see below) |
| ParticipantIdentity | `u5` | one per account |

Instance ids never show up in `/changes`; only the stored event does.

## Calendar

| Property | |
| --- | --- |
| `id`, `name`, `description` | `name` 1–255 bytes, `description` up to 10 000 bytes or `null` |
| `color` | CSS `#rrggbb`, or `null`. CalDAV keeps Apple's `#RRGGBBAA`; `#rgb` and `#rrggbb` are accepted when writing, colour names are not |
| `sortOrder` | 0 to 2³¹−1 |
| `isSubscribed` | always `true` |
| `isVisible` | whether the webmail and the apps show its events; kept on the server, CalDAV does not know it |
| `isDefault` | exactly one calendar is the default |
| `includeInAvailability` | `"all"`, `"attending"` or `"none"`: which of its events make the account busy (see [Availability](#availability)). By default `"all"` for one's own calendars and `"none"` for subscribed ones and those shared with the account. CalDAV clients see it as `schedule-calendar-transp` (`opaque` or `transparent`) and may set it there |
| `defaultAlertsWithTime`, `defaultAlertsWithoutTime` | the alerts of events that use the defaults, see [Default alerts](#default-alerts); `null` for none |
| `shareWith` | who else sees it; see [Shared calendars](#shared-calendars) |
| `timeZone` | an IANA name or `null`; stored as the CalDAV `calendar-timezone` |
| `myRights` | everything `true` for one's own calendars; `mayDelete` is `false` for the only own calendar. A subscribed calendar ([calendar-import.md](calendar-import.md)) has `mayWriteAll`, `mayWriteOwn`, `mayUpdatePrivate` and `mayRSVP` `false`: only its feed changes its events. For shared ones see above |
| `uwuSharedBy` | the owner of a calendar shared with the account, else `null` |

For a calendar shared with the account, `name`, `color`, `sortOrder`,
`isVisible`, `timeZone`, `includeInAvailability` and the default alerts are
its own ([Per-user properties](#per-user-properties)).

`Calendar/get` and `Calendar/changes` are standard. `Calendar/set` creates,
changes and destroys calendars with the properties above; a property with only
one possible value may be sent with that value. Also:

- `onDestroyRemoveEvents`: without it a calendar that still holds entries
  (events or tasks) is not destroyed (`calendarHasEvent`).
- The only calendar cannot be destroyed (`forbidden`). When the default one
  goes, the first of the others becomes the default.
- `onSuccessSetIsDefault` makes a calendar the default when everything else in
  the call worked; both calendars whose `isDefault` changed are reported in
  `created` or `updated`.

## Default alerts

`defaultAlertsWithTime` and `defaultAlertsWithoutTime` of a calendar are maps
of at most 20 alerts that trigger relative to the event (`OffsetTrigger`, an
`AbsoluteTrigger` is `invalidProperties`), with ids that are unique across
the account's calendars. They are everyone's own, like the other per-user
properties.

An event with `useDefaultAlerts: true` gets the defaults of its calendar
(those without time for an all-day event) in place of its own `alerts`. For
one's own calendars the server writes them into the event as its VALARMs,
each with the default alert's id, so phones ring them without knowing about
defaults: whenever the event is stored over JMAP, and again in every such
event of the calendar when its owner changes the defaults. What an alert keeps
for itself stays: the time it was `acknowledged`, and alerts that snooze it
(`relatedTo` it). CalDAV clients see an event's `useDefaultAlerts` as a
`JSPROP` and keep it.

CalDAV clients see and set the same defaults as the calendar's
`default-alarm-vevent-datetime` and `default-alarm-vevent-date` properties
(VALARMs), as Apple's calendar does; an alarm set there without an id gets a
new one.

## CalendarEvent

### CalendarEvent/get

Standard `/get` with the draft's arguments: `timeZone` (IANA, default
`Etc/UTC`) for floating events, `recurrenceOverridesAfter` and
`recurrenceOverridesBefore` (only overrides whose recurrence id lies in
between), and `reduceParticipants` (only the owners and the account itself).
An event with `hideAttendees` shows someone who is not one of its owners the
same way. `ids: null` works up to `maxObjectsInGet` events.

Without `properties` every stored property comes back except `iCalendar`; with
`properties` only those. `id`, `calendarIds`, `isDraft`,
`isOrigin` and `baseEventId` are always there. `utcStart` and `utcEnd` are
computed when asked for (not together with `recurrenceOverrides`). `isOrigin`
is `true` unless `organizerCalendarAddress` names someone who is not one of
the account's addresses.

A timed event:

```json
{
  "id": "v1",
  "@type": "Event",
  "uid": "c3566f06-58ec-4c0c-9a50-d78270c6b9b2",
  "calendarIds": { "c1": true },
  "isDraft": false,
  "isOrigin": true,
  "baseEventId": null,
  "title": "Tierarzt",
  "description": "Impfung",
  "locations": { "1": { "@type": "Location", "name": "Praxis am Markt" } },
  "start": "2026-10-20T09:00:00",
  "timeZone": "Europe/Berlin",
  "duration": "PT1H",
  "sequence": 0,
  "created": "2026-09-23T09:17:45Z",
  "updated": "2026-09-23T09:17:45Z"
}
```

A property at its JSCalendar default is often left out, `showWithoutTime:
false` for example. An all-day event is floating, starts at midnight and lasts
whole days:

```json
{
  "id": "v4",
  "@type": "Event",
  "uid": "c5e72a9d-ed41-4cec-ab8f-60d16ef63adb",
  "calendarIds": { "c1": true },
  "isDraft": false,
  "isOrigin": true,
  "baseEventId": null,
  "title": "Urlaub",
  "start": "2026-12-24T00:00:00",
  "duration": "P3D",
  "showWithoutTime": true,
  "sequence": 0,
  "created": "2026-09-23T09:17:45Z",
  "updated": "2026-09-23T09:17:45Z"
}
```

A series keeps its rule and the instances that differ:

```json
"recurrenceRule": { "frequency": "weekly", "count": 5 },
"recurrenceOverrides": {
  "2026-10-27T09:00:00": { "start": "2026-10-27T10:00:00", "title": "Yoga im Park" },
  "2026-11-03T09:00:00": { "excluded": true }
}
```

and one of its instances, by its instance id, is the series with the
instance's changes applied, its own `start` and `recurrenceId`, and no rule:

```json
{
  "id": "v1_20261027T090000",
  "baseEventId": "v1",
  "calendarIds": { "c1": true },
  "isDraft": false,
  "isOrigin": true,
  "title": "Yoga im Park",
  "start": "2026-10-27T10:00:00",
  "timeZone": "Europe/Berlin",
  "duration": "PT1H",
  "recurrenceId": "2026-10-27T09:00:00",
  "recurrenceIdTimeZone": "Europe/Berlin",
  "recurrenceRule": null,
  "recurrenceOverrides": null,
  "…": "the other properties of the series"
}
```

### CalendarEvent/set

Standard `/set` with `ifInState`.

**create** takes a JSCalendar Event. `calendarIds` names exactly one of the
account's calendars. `null` at the top is the same as leaving a property out.
The server fills in `@type`, a UUID `uid`, `sequence` 0, `created` and
`updated` when they are missing and returns them in `created` together with
`id`, `isDraft`, `isOrigin` and `baseEventId`. A `uid` that another event of
the calendar's owner already has is `alreadyExists`, with its `existingId`.

**update** is a JMAP PatchObject applied to the event's JSCalendar, which is
then written back as iCalendar. `calendarIds` (whole, or
`calendarIds/<id>`) moves the event to another calendar; it keeps its id.
`uid`, `@type`, `id`, `isOrigin` and `baseEventId` cannot change. When
something other than `calendarIds`, `isDraft`, `updated`, `sequence`,
`keywords`, `color`, `freeBusyStatus`, `useDefaultAlerts` or `alerts` changes,
`sequence` goes up by one (unless the patch sets a higher one); `updated` is
set to now when the account is the origin. `updated` in the response names
what the server set.

`utcStart` and `utcEnd` may be given instead of `start` and `duration`; an
event without `timeZone` then gets the calendar's, or `Etc/UTC`.

**Instance ids** work too: an update of an instance becomes that instance's
override on the series (only what differs from the series is kept), a destroy
excludes the instance (an `EXDATE` for CalDAV clients). The rule, the
overrides, `calendarIds` and `uid` of an instance cannot change on their own.

Checked before anything is stored (`invalidProperties` names the property):

- `@type` is `Event`, `uid` is 1–255 bytes, there is no `method`.
- `start` is a LocalDateTime; `start`, the end, `until` and every recurrence
  id lie between `minDateTime` and `maxDateTime`.
- `timeZone` (also in overrides) is an exact IANA name or one of the event's
  custom `timeZones` (at most 10, each with 1 to 20 yearly rules).
- `duration` is a JSCalendar Duration without fractions (up to 100 years).
- An event with `showWithoutTime` starts at `T00:00:00` and lasts whole days.
- `title` is at most 1024 bytes; the whole event as iCalendar at most 1 MiB
  (`tooLarge`).
- `recurrenceRule` is one rule with known parts (frequency, interval up to
  10 000, count up to 1 000 000 or until, byDay, byMonth, byMonthDay, …);
  `recurrenceRules` and `excludedRecurrenceRules` are refused, and so is a
  top-level `recurrenceId` next to a rule or overrides.
- An override may not change what belongs to the series (`uid`,
  `recurrenceRule`, `privacy`, …); a series has at most 1000 changed or
  excluded instances.
- `isDraft` may only be `true` when the event is created.

Everything else in an event is kept as data, unknown properties included.

`sendSchedulingMessages: true` sends what the change means to the others
(iTIP): an organizer's new or changed event invites its participants, someone
taken off or a destroyed event cancels, and an attendee's changed
`participationStatus` (or destroying the invitation, which declines it)
answers the organizer. People of this server get it straight into their own
calendars, everyone else by mail; see [calendars.md](calendars.md). A new
event with participants and no `organizerCalendarAddress` gets the account's
address as its organizer. Messages are only sent for events in the account's
own calendars, never for calendars shared with it. Without
`sendSchedulingMessages`, participants are simply stored and nobody hears
about it, as the draft says.

An invitation that reached the account shows up as an event of its default
calendar with `isOrigin: false`, the organizer's `organizerCalendarAddress`
and the account's participant at `participationStatus: "needs-action"`. To
answer, patch that participant's `participationStatus` (`accepted`,
`declined`, `tentative`) with `sendSchedulingMessages: true`.

**Single instances without their series.** Someone invited to some instances
of a series only has them without the series: VEVENTs with a
`RECURRENCE-ID` and no rule. Such an event has its `recurrenceId` (and
`recurrenceIdTimeZone`) and no `recurrenceRule`, as the draft has it, and a
JMAP client may create one the same way. CalDAV keeps all instances of one
uid in one object, so when there are several, the event's id stands for the
earliest one and the others have the ids of instances (`v34_20261103T090000`,
with `baseEventId` naming the event): `/get` and `/set` take them, a query
with `expandRecurrences` lists them, and destroying one takes it out of the
object. Destroying the event itself deletes the whole object, as it is one
for CalDAV.

**One calendar per event.** `maxCalendarsPerEvent` is 1: CalDAV keeps an
event as one object in one calendar collection, and a second copy of it in
another calendar would be a second object with the same uid, which phones
would show twice and which would drift apart the first time either is
changed. So `calendarIds` names exactly one calendar; naming more is
`invalidProperties`. Moving an event keeps its id.

**Custom time zones** (RFC 8984, section 4.7.2) come from VTIMEZONEs that
name no zone of the IANA database (calcard maps IANA names, Windows names and
`X-LIC-LOCATION` to IANA zones by itself). Such an event has `timeZone:
"/<TZID>"` and the zone's rules in `timeZones`; `utcStart`, queries and
expanded instances follow those rules. A client may define its own zone the
same way, and it is written back as a VTIMEZONE that phones understand. The
rules of a custom zone are yearly, with extra onsets (RDATE), as every
VTIMEZONE in use has them.

A new event with `isDraft: true` is a draft: it is stored and CalDAV clients
see it like any other event, but no scheduling message goes out for it, over
JMAP or CalDAV, whatever changes it. Setting `isDraft` to `false` makes it an
event, and with `sendSchedulingMessages` its participants are then invited as
for a new one. A draft stays one until then; an event never becomes a draft
again (`invalidProperties`). Deleting a draft tells nobody either.

### CalendarEvent/query

Filters: `inCalendar`, `after`, `before` (LocalDateTimes in the `timeZone`
argument, IANA, default `Etc/UTC`), `text`, `title`, `description`,
`location`, `owner`, `attendee` and `uid`, also combined with `AND`, `OR` and
`NOT`. Texts are matched case-insensitively; every word has to be there, and
`"quoted phrases"` as a whole.

Sort by `start`, `uid`, `recurrenceId`, `created` or `updated`; without a sort,
by start. `position` (also negative), `anchor`, `anchorOffset`, `limit` (at
most 5000) and `calculateTotal` work as in RFC 8620.

With `expandRecurrences: true` the filter has to be one condition with
`after` and `before`, at most `P400D` apart (`expandDurationTooLarge`
otherwise). Every instance of a series in the window gets its own id; events
that do not repeat keep theirs. A series is expanded for its first 10 000
occurrences, so an endless daily series shows for about 27 years; a query
that spends more than five seconds expanding stops with
`cannotCalculateOccurrences`.

All calendar-event calls of one request share fifteen seconds between them.
What comes after is refused: `/get` with `serverUnavailable`, `/query` with
`cannotCalculateOccurrences`, and each further object
of `/set` with `rateLimit`, so a client sends the rest in a new request.

`CalendarEvent/queryChanges` works as in RFC 8620 (`canCalculateChanges:
true`): every event that changed since the query state is removed, and added
again at its place where it matches now. With `expandRecurrences` the same
holds for instances: those a changed event has now are removed and added
again, and those it had at the query state in the query's time window are
removed too — whatever the query's text conditions, so the old text of an
event, private or in a calendar no longer shared, decides nothing; a secret
event only counts for its calendar's owner. For that the
server keeps what recurring events were before each change, for 30 days, at
most 500 changes per account and none of an event over 128 KiB; a query state
older than what is kept, or from before 0.17, answers `cannotCalculateChanges`,
and the client queries anew.

### CalendarEvent/parse

Turns blobs of iCalendar, uploads or `.ics` attachments of mail (their part
blob ids from `Email/get`), into CalendarEvents without storing anything, the
same way stored events are read (custom time zones and single instances
included). Every event of a file comes back, at most 1000 per blob, and a
blob may have up to 4 MiB. `id`, `baseEventId`, `calendarIds`, `isDraft` and
`isOrigin` are `null`. A blob that is no iCalendar with events is in
`notParsable`. To keep an event, create it with `CalendarEvent/set`.

### CalendarEvent/copy

Standard `/copy`, with one difference: every calendar the login sees, its own
and those shared with it, is in its own account here, so `fromAccountId` is
the account itself (another is `fromAccountNotFound`). Each `create` names
the event (or one of its instances, which becomes an event of its own) by
`id` and may set any property, `calendarIds` to copy it into another
calendar. The copy keeps the uid unless the create gives another; a uid the
calendar's owner already has is `alreadyExists` with the `existingId`, as for
`/set`. `onSuccessDestroyOriginal` destroys the originals in a
`CalendarEvent/set` after it. An event its owner keeps `private` cannot be
copied by others (`forbidden`).

### CalendarEvent/changes

Standard. The state is the account's change number, shared with mail.

## CalendarEventNotification

When someone else changes an event the account sees, it gets a
CalendarEventNotification (id `n12`): a person it shares a calendar with, or
who shares one with it, over JMAP or CalDAV, and scheduling that puts an
invitation, an update, a cancellation or an answer into its calendars. The
account's own changes leave none for itself. `changedBy` names who it was
(`principalId` for people of this server, never by a masked address;
`calendarAddress` and the message's `COMMENT` for scheduling), `event` is the
event before the change (after it for `created`), `eventPatch` what changed at
its top level, and `isDraft` whether it is a draft. For an event over 128 KiB
the notification says who changed it, without `event` and `eventPatch`.

An event its owner keeps `private` or `secret` is only news to the owner.
Nothing is noted for a calendar filled from a subscription, for imports, or
when the server writes default alerts into events. Each account keeps its
newest 200 notifications for at most 30 days.

`/get`, `/changes`, `/query` (filters `after`, `before`, `type`,
`calendarEventIds`; sorted by `created`) and `/queryChanges` are standard.
`/set` only destroys, which dismisses a notification; `create` and `update`
are `forbidden`.

## ParticipantIdentity

One per account: `{ "id": "u5", "name": <display name>, "calendarAddress":
"mailto:<login>", "isDefault": true }`. `/get` and `/changes` are standard;
every change in `/set` is `forbidden`.

## Alerts

Besides handing alerts to CalDAV clients, which ring them, the server fires
them itself (draft section 6): for each account that sees an event, from the
alerts as that account sees them (its own ones in a calendar shared with it,
default alerts included), at the time of the next instance each alert goes off
for, and not again once `acknowledged` covers it. Drafts ring for nobody.

- An alert with `"action": "display"` (or none) is pushed as a
  `CalendarAlert` — `accountId`, `calendarEventId` (the stored event, never an
  instance id), `uid`, `recurrenceId` and `alertId` — to the EventSource (as
  the event `calendarAlert`), the WebSocket and Web Push subscriptions whose
  types include `CalendarAlert` (or are `null`).
- An alert with `"action": "email"` puts a short reminder mail into the
  account's inbox, in the language it chose, from `postmaster@` its domain.
  One event sends at most one such mail in four minutes, and one account gets
  at most 100 a day, so a series that repeats every minute cannot fill an
  inbox.

The server looks every 20 seconds and rings at most 20 alerts of one event
for one account at a time, the earliest. An alert that should have gone off more
than an hour ago, because the server was down, is dropped rather than
delivered late.

## Push

`Calendar`, `CalendarEvent`, `CalendarEventNotification` and
`ParticipantIdentity` are push types of the EventSource, next to the mail
types, and `CalendarAlert` pushes alerts (see [Alerts](#alerts)).

## Not supported

Nothing of the draft is left out. Where it leaves room, this server chooses
as described above; the two choices a client notices are that calendars
shared with the account are part of the account itself rather than of
accounts of their owners (so `CalendarEvent/copy` copies within it), and that
an event is in exactly one calendar (see *One calendar per event* under
[CalendarEvent/set](#calendareventset)).
