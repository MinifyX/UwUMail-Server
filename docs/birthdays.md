# Birthdays

Every account gets a calendar of its own made from the birthdays and
anniversaries in its contacts: **Geburtstage** or **Birthdays**, depending on
the person's language. Phones, Thunderbird and the webmail see it like any
other calendar, over CalDAV and JMAP. Nobody writes into it: it changes when a
contact changes. Birthdays that other calendars kept as events can be moved
into the contacts with the JMAP extension `urn:uwumail:jmap:birthdays`, which
the webmail and the UwUMail apps use.

## Where the dates come from

- **Birthday**: the card's `BDAY` (JSContact `anniversaries` of kind `birth`),
  with or without a year: `1996-04-12`, `19960412`, `--04-12`, `--0412`, and
  Apple's `1604-04-12` with `X-APPLE-OMIT-YEAR` or `0000-04-12` for "no year".
- **Anniversary**: `ANNIVERSARY` (kind `wedding`), shown as *Hochzeitstag von
  Max Muster* / *Wedding anniversary of Max Muster*.
- **Other dates** that Apple and Google write as `itemN.X-ABDATE` with an
  `X-ABLabel`: `_$!<Anniversary>!$_` is a wedding anniversary, every other
  label becomes the title (*Kennenlerntag von Max Muster*). They are shown and
  go into the calendar; the webmail edits the birthday and the anniversary.
- A date given as text (`VALUE=text`, "circa 1800"), a day that does not exist
  (31 April, 29 February 2023) and groups (`KIND:group`) are left out. A card
  gives at most ten dates.

**Whose contacts:** only the account's **own** address books, all of them. A
card in an address book someone else shares with the account is in the
owner's birthdays calendar, not in the reader's: otherwise everyone sharing a
family address book would see the same birthday once per share, and nobody
could turn it off. Moving a card into another person's address book moves its
dates into their calendar.

## The calendar

- Made with the first date a card gets (and, once, for everyone who had dates
  before 0.18.0, when the server starts). Its URL segment is `birthdays`
  (`birthdays-<random>` when the person already had a calendar called that).
- **Read-only**: CalDAV `PUT`/`DELETE` of its entries answers `403`, JMAP
  `CalendarEvent/set` answers `readOnly` (create, update, destroy). The
  calendar itself cannot be deleted (`forbidden`) and never becomes the
  default one; it does not count as the account's only calendar either. Its
  name, colour, order, visibility and time zone are the person's to change,
  like those of a subscribed calendar. `includeInAvailability` is `"none"` by
  default: birthdays never make anyone busy.
- It follows the language the person chose (or the server's, `tone.language`,
  when they left it to the server): German for `de`, English for every other.
  After a change the calendar is written anew the next time the calendars are
  listed, and renamed unless the person named it themselves.
- Each date is one **yearly all-day event** without an end
  (`bday-<card>-b.ics` for the birthday, `bday-<card>-aN.ics` for the others),
  transparent, starting in the year it happened (years before 1900 start in
  1900, dates without a year in 1970).
- Every write of a card, over CardDAV or JMAP, changes its events in the same
  transaction, so ETags, CTags, sync tokens, JMAP states and push move
  together. A card write that does not change a date changes nothing in the
  calendar. With a full calendar (50 000 entries) or no room for another
  calendar (100 collections), the card is stored and its dates left out.

### Titles and the age

A CalDAV client sees one event per date with an unchanging title, because a
recurring event has one title for every year:

| | German | English |
| --- | --- | --- |
| birthday with year | `Max Muster (*1996)` | `Max Muster (b. 1996)` |
| birthday without | `Max Muster` | `Max Muster` |
| anniversary with year | `Hochzeitstag von Max Muster (seit 2021)` | `Wedding anniversary of Max Muster (since 2021)` |

The description says it too ("… geboren 1996. Aus deinen Kontakten.").

Over JMAP, each **instance** of an expanded query (`CalendarEvent/query` with
`expandRecurrences`, then `CalendarEvent/get` of the instance ids) has the age
of its year in its title: `Max Muster (30)`,
`Hochzeitstag von Max Muster (5 Jahre)`. The first year (age 0) and years
before it have the plain name. Every event of the calendar also has:

```json
"uwuBirthday": { "contactId": "k12", "kind": "birth", "label": null, "name": "Max Muster", "year": 1996 }
```

`kind` is `birth`, `wedding` or `other` (`label` then holds its label);
`year` is `null` without one. Clients that expand recurrences themselves use
it to show the age ("wird 30" / "turns 30") and to open the contact.

### 29 February

A birthday on 29 February falls on **28 February** in years without one: the
event's rule is `FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1`, the last day of
February, which every CalDAV client expands the same way. People born on a leap
day mostly celebrate on the 28th, and a reminder on 1 March would be late. The
age counts from that day on.

## Reminders

Reminders are set per contact and are off by default. A card keeps them as
lines of its own, which CardDAV clients leave as they are:

```
X-UWUMAIL-REMINDER:0 09:00
X-UWUMAIL-REMINDER:7 09:00
```

— days before the day (0 to 28) and the time of day, at most five per card.
JMAP `ContactCard` shows them as

```json
"uwuReminders": [{ "daysBefore": 0, "time": "09:00" }, { "daysBefore": 7, "time": "09:00" }]
```

and takes them back the same way (`null` or `[]` for none; anything else is
`invalidProperties`). The webmail offers *on the day at 9:00*, *1 day before*
and *1 week before*, in any combination.

Each reminder becomes a `VALARM` of every event of the card (`TRIGGER:-PT15H`
for a day before at nine), so phones ring on their own, and the server's alert
worker fires it as a JMAP `CalendarAlert` or mail like any other
([jmap-calendars.md](jmap-calendars.md#alerts)). The time is that of the
birthdays calendar's time zone, which it takes from the default calendar when
it is made and which the person can change.

## Moving birthdays out of other calendars

Many people kept birthdays as yearly events in an ordinary calendar. The
extension `urn:uwumail:jmap:birthdays` finds them, matches them to contacts
and moves them there, after which the event is deleted and the birthdays
calendar shows the date instead.

### Capability

In the session's `capabilities` (an empty object) and the account's
`accountCapabilities`, when the account has calendars and contacts and the
credential may reach them (scope `dav`):

```json
"urn:uwumail:jmap:birthdays": { "maxImport": 500, "maxCandidates": 1000 }
```

### What counts as a birthday event

An event of one of the calendars the account sees (not the birthdays calendar
itself) that is **all day** and **yearly** (`FREQ=YEARLY`, every year), or
**marked** as a birthday by the program that made it: a property named like
one (KDE's `X-KDE-KABC-BIRTHDAY`), the category *Birthday*/*Geburtstag*
(Google's and many phones' exports) or a uid containing "birthday". Its title
has to say "birthday" unless it is marked:

- German: `Geburtstag von Max`, `Geburtstag: Max`, `Max Geburtstag`,
  `Geb. Max`, `Max hat Geburtstag`
- English: `Max's birthday`, `Max’s Birthday!`, `Birthday of Max`,
  `bday Max`, `b-day`, `Happy birthday Max`
- `🎂 Max`, `Max 🎂` (also 🎉 🎈 🎁 🥳 🍰 🧁)

The rest of the title is the name. A four-digit year in it is the year of birth
(`Geburtstag Max (*1990)`, `Max (1990)`); an age (`Max (30)`) is not. The start
date of a marked event also gives the year, since address books write the year
of birth there; the start of an unmarked one does not (people make a yearly
event in the year they make it). `Geburtstagsfeier` (a party) is no birthday.

### Matching names

Names are compared by characters, in lower case, with umlauts both spelled out
and plain (`Müller` = `Mueller` = `Muller`, `ß` = `ss`) and other accents
dropped (`Zoë` = `Zoe`). A name matches a contact whose full name, given name
and surname either way round, or nickname is the same; when none is, a contact
whose names hold every word of it (`Max` for `Max Muster`). Only contacts the
account may change are candidates.

### Birthdays/scan

`Birthdays/scan {accountId}` → `{accountId, candidates, truncated}`, where each
candidate is

| Property | |
| --- | --- |
| `eventId`, `calendarId`, `title` | the event |
| `name` | the name read from the title |
| `birthday` | `{month, day, year}`, `year` `null` without one |
| `marked` | the event was marked as a birthday |
| `mayDeleteEvent` | the event can be deleted afterwards; `false` in a subscribed calendar or one shared for reading, whose events stay |
| `match` | `matched`: one contact, without a birthday or with the same day and no year yet; `known`: one contact that has this birthday already; `conflict`: one contact with another birthday; `ambiguous`: several contacts; `unmatched`: none |
| `contacts` | up to 10 `{contactId, addressBookId, name, birthday}`, the match first |

At most 20 000 events are looked at and 1 000 candidates returned;
`truncated` says there were more.

### Birthdays/import

```json
["Birthdays/import", { "accountId": "a1", "entries": {
  "v17": { "contactId": "k12" },
  "v18": { "newContact": { "name": "Leni Muster", "addressBookId": null } },
  "v19": { "contactId": "k40", "overwrite": true, "deleteEvent": false }
} }, "0"]
```

→ `{accountId, imported: {eventId: {contactId, created, eventDeleted}},
notImported: {eventId: SetError}}`. Up to 500 entries per call.

- Each entry gives exactly one of `contactId` and `newContact` (`name`, and an
  `addressBookId` or the default address book). Skipping an event is leaving it
  out.
- The server reads the date from the event again; the client never sends it.
  It writes it into the card's `BDAY` (everything else of the card stays as
  written) or makes a new vCard 3.0 card, and then deletes the event — **in one
  transaction**: when the card cannot be written, the event stays.
  `deleteEvent: false` keeps it; `eventDeleted` is `false` when its calendar
  does not allow deleting it.
- A contact with another birthday is only changed with `overwrite: true`
  (`birthdayExists` otherwise). One with the same day and no year gets the
  year; one that has the birthday already is left alone and only the event
  goes.
- Errors: `notFound` (event or contact), `notABirthday`, `birthdayExists`,
  `invalidProperties`, `stateMismatch` (changed meanwhile; scan again),
  `overQuota`.

The webmail shows the plan before anything happens: what goes where
automatically, a choice per unclear entry (*Neuen Kontakt anlegen*,
*Bestehendem Kontakt zuordnen*, *Überspringen*) and which events will be
deleted.
