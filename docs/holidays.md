# Holidays and week numbers

Two calendars the server makes itself when someone turns them on: **public
holidays** for a country or region, and **week numbers**. Both are worked out
on the server (Easter by the Gregorian computus), with no network and no feed.
They are off until switched on in My account → Calendars, in the webmail or
over JMAP `UserSettings`.

## What they are like

- Like a subscribed calendar: read-only over CalDAV and JMAP, never the
  default calendar, no free/busy (`includeInAvailability` `"none"`), no alarms.
  Events are all-day and `TRANSPARENT`.
- Name and colour can be changed. A changed name stays when the region
  changes; the default name follows the region (*Feiertage Bayern*,
  *Holidays Bayern*; regions keep their local name) and the person's language.
- They cannot be deleted; turning the setting off deletes the calendar and its
  events. Turning it on again makes a new one.
- They span the previous year and the next three. The hourly cleanup moves the
  span on at the turn of the year.

## Settings

| Key | Value |
|---|---|
| `holidays` | `{ "country": "DE" \| "AT" \| "CH", "region": <ISO 3166-2 code of that country> \| null }`; `null` or not set is off. `region` `null` gives only the nationwide holidays |
| `weekNumbers` | `true` or `false` |

Over JMAP they are keys of `UserSettings` ([jmap-settings.md](jmap-settings.md));
writing them creates, changes or deletes the calendar in the same write, and
the calendar carries `uwuBuiltin` `"holidays"` or `"weeks"`
([jmap-calendars.md](jmap-calendars.md)).

The portal and the webmail use:

| Request | |
|---|---|
| `GET /api/account/calendars` | also `builtin: { holidays, weekNumbers }`, and `builtin` (`"holidays"`, `"weeks"` or `null`) on each calendar |
| `PUT /api/account/calendars/builtin` | `{ "holidays"?, "weekNumbers"? }` as above; answers with the calendars view |
| `GET /api/account/calendars/catalog` | `{ countries: [{ code, names: { de, en } }], regions: [{ code, name }], subscriptions: [...] }` |

## Germany

The nationwide holidays (New Year, Good Friday, Easter Monday, 1 May,
Ascension, Whit Monday, 3 October, Christmas Day and Boxing Day) and those of
the 16 states:

- Epiphany (6 January): BW, BY, ST
- International Women's Day (8 March): BE, MV
- Corpus Christi: BW, BY, HE, NW, RP, SL
- Assumption (15 August): SL, and BY with the note that it is a holiday only
  in municipalities with a mainly Catholic population
- World Children's Day (20 September): TH
- Reformation Day (31 October): BB, HB, HH, MV, NI, SN, ST, SH, TH
- All Saints' Day (1 November): BW, BY, NW, RP, SL
- Day of Repentance and Prayer (Wednesday before 23 November): SN
- Easter Sunday and Whit Sunday: BB

Left out: the Augsburg Peace Festival (8 August, city of Augsburg only) and
Corpus Christi in the Catholic municipalities of Saxony and Thuringia.

## Austria

The national public holidays: New Year, Epiphany, Easter Monday, 1 May,
Ascension, Whit Monday, Corpus Christi, Assumption, National Day (26 October),
All Saints, Immaculate Conception, Christmas Day and St Stephen's Day. There
are no regions.

## Switzerland

Only 1 August is a federal holiday; everything else is cantonal. The server
gives New Year, Ascension and Christmas Day everywhere and, per canton, a
rough set: Berchtoldstag, Good Friday, Easter and Whit Monday, Corpus Christi,
Assumption, All Saints, Immaculate Conception, St Stephen's Day, St Joseph,
1 May, the Näfels Fahrt (GL), Jeûne genevois and Restauration (GE), Lundi du
Jeûne (VD) and others.

**This is an approximation.** Whether a day is a public holiday in Switzerland
also depends on the municipality, and some days count only as days of rest
under labour law. The portal says so next to the canton.

## Week numbers

ISO 8601 weeks: one all-day event per week from Monday to Sunday, titled
*KW n* in German and *Week n* otherwise. Years with 53 weeks (2026) get KW 53.

## Subscription catalogue

`crates/uwumail-web/src/calendar_catalog.json` is shipped with each release
and lists calendar feeds the portal and the webmail offer under "Subscribe to a
calendar", for school holidays for instance. Only sources go in whose licence
allows it and whose address was checked; each entry names its licence and the
day it was checked. It is empty for now: the sources checked on 2026-11-14
(mehr-schulferien.de, the OpenHolidays API, schulferien.org) either forbid
redistribution, state no licence or have no lasting feed address. The file
lists them and why.
