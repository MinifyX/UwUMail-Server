# JMAP Calendars

UwUMail Server serves the calendars people already have over CalDAV as JMAP
Calendars ([draft-ietf-jmap-calendars](https://datatracker.ietf.org/doc/draft-ietf-jmap-calendars/),
in the RFC editor queue). The webmail and the UwUMail apps use it; phones and
Thunderbird keep using CalDAV, and both see the same calendars and events.

This page says what is supported and how the server behaves where the draft
leaves room.

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

Only accounts that may use calendars get it: when an admin switches CalDAV off
for an account (services have it off from the start), the capability is gone
from its session and every calendar method answers
`accountNotSupportedByMethod`.

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

## Ids

| Object | Id | |
| --- | --- | --- |
| Calendar | `c12` | |
| CalendarEvent | `v34` | a stored event, a series included |
| An instance of a series | `v34_20261027T090000` | the event id and the instance's `recurrenceId` without `-` and `:`; only from `CalendarEvent/query` with `expandRecurrences` |
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
| `includeInAvailability` | always `"all"` |
| `defaultAlertsWithTime`, `defaultAlertsWithoutTime`, `shareWith` | always `null` |
| `timeZone` | an IANA name or `null`; stored as the CalDAV `calendar-timezone` |
| `myRights` | everything `true` except `mayShare` (nothing is shared); `mayDelete` is `false` for the only calendar |

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

## CalendarEvent

### CalendarEvent/get

Standard `/get` with the draft's `timeZone` argument (IANA, default
`Etc/UTC`) for floating events. `recurrenceOverridesBefore`/`After` and
`reduceParticipants` are ignored. `ids: null` works up to `maxObjectsInGet`
events.

Without `properties` every stored property comes back except `iCalendar`; with
`properties` only those. `id`, `calendarIds`, `isDraft` (always `false`),
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
the account already has is `alreadyExists`.

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
- `timeZone` (also in overrides) is an exact IANA name; custom time zones are
  not supported.
- `duration` is a JSCalendar Duration without fractions (up to 100 years).
- An event with `showWithoutTime` starts at `T00:00:00` and lasts whole days.
- `title` is at most 1024 bytes; the whole event as iCalendar at most 1 MiB
  (`tooLarge`).
- `recurrenceRule` is one rule with known parts (frequency, interval up to
  10 000, count up to 1 000 000 or until, byDay, byMonth, byMonthDay, …);
  `recurrenceRules`, `excludedRecurrenceRules` and a top-level `recurrenceId`
  are refused.
- An override may not change what belongs to the series (`uid`,
  `recurrenceRule`, `privacy`, …); a series has at most 1000 changed or
  excluded instances.
- `isDraft` may only be `false`.

Everything else in an event is kept as data, unknown properties included.

`sendSchedulingMessages: true` is refused with `noSupportedScheduleMethods`
when the event has participants other than the account itself; without it,
participants are simply stored.

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

`CalendarEvent/queryChanges` answers `cannotCalculateChanges`.

### CalendarEvent/changes

Standard. The state is the account's change number, shared with mail.

## ParticipantIdentity

One per account: `{ "id": "u5", "name": <display name>, "calendarAddress":
"mailto:<login>", "isDefault": true }`. `/get` and `/changes` are standard;
every change in `/set` is `forbidden`.

## Push

`Calendar`, `CalendarEvent` and `ParticipantIdentity` are push types of the
EventSource, next to the mail types.

## Not supported

- Sharing, principals and `Principal/getAvailability`
  (`urn:ietf:params:jmap:principals:availability`)
- Scheduling: no invitations, replies or cancellations are sent (iTIP/iMIP)
- `CalendarEventNotification`, `CalendarEvent/copy`, `CalendarEvent/parse`
  (`urn:ietf:params:jmap:calendars:parse`)
- Default alerts, `useDefaultAlerts` and alerts pushed by the server; alerts
  are stored and handed to CalDAV clients, which ring them
- Drafts (`isDraft: true`), more than one calendar per event, custom time
  zones, events that are single instances without their series
- Per-user properties: there is one user per calendar anyway
- `CalendarEvent/queryChanges`
