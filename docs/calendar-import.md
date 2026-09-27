# Bringing calendars and contacts over

Moving to your own server should not mean typing birthdays in again. Under
**My account → Calendars & contacts → Bring them over** everyone can take
their calendars and contacts along from wherever they were, in three ways:

| Way | For | Afterwards |
| --- | --- | --- |
| [Import a file](#importing-a-file) | an `.ics` or `.vcf` file any provider or app exports | ordinary calendars and address books, yours to change |
| [Subscribe to a calendar](#subscribing-to-a-calendar) | an iCal address: holidays, a club's dates, a Google calendar | a read-only calendar the server fetches again regularly |
| [Move from another provider](#moving-from-another-provider) | everything at iCloud, WEB.DE, GMX, Posteo, mailbox.org, Nextcloud or any CalDAV/CardDAV server | ordinary calendars and address books, one for each there |

Whatever comes in shows up in every calendar and contacts app over CalDAV and
CardDAV, and in the webmail and the UwUMail apps over JMAP. Taking things over
never sends invitations: the people in an imported meeting hear nothing.

## Importing a file

An iCalendar file (`.ics`) goes into a calendar, a vCard file (`.vcf`) into an
address book: a new one, named after the calendar in the file or the file
itself, or one you already have. Up to 20 MB per file.

The file is cut into single entries the way CalDAV keeps them: an event with
all its exceptions, each with the time zones it uses. vCards of version 2.1, as
old phones and Outlook write them, become vCard 3.0 on the way, with their
umlauts decoded. An entry without a UID gets one that stays the same for the
same entry, so importing a file twice changes nothing twice. By default an
entry that is there already is overwritten with what the file says; *Only add
new entries* leaves it alone.

The report says what was left out and why: an entry the checks refuse (the
same ones that apply to what apps store), one that is too large (1 MB), one
whose UID another of your calendars has already (JMAP keeps UIDs unique across
your calendars), and duplicates within the file, where the later one counts.

On the command line, for an admin moving people over:

```bash
uwumail-server import ics  ferien.ics  --account mini@example.org                 # a new calendar
uwumail-server import ics  work.ics    --account mini@example.org --calendar personal
uwumail-server import vcf  handy.vcf   --account mini@example.org --address-book contacts --only-new
cat export.vcf | uwumail-server import vcf - --account mini@example.org --name "Old phone"
```

`--dry-run` only counts. Running it again is safe, as in the portal.

## Subscribing to a calendar

A subscribed calendar is filled by its address alone. The server fetches it
again every 15 minutes, hour, 6 hours or day (the calendar's own
`REFRESH-INTERVAL` is the default when it names one), asks only for news when
the publisher supports it (ETag, Last-Modified), and makes the calendar hold
exactly what the feed holds: what is new appears, what changed is rewritten,
what went is removed, and an entry that differs only in its time stamp is left
alone. Clients see it through CalDAV sync and JMAP push like any other change.

`webcal://` addresses work too; they are https. Only addresses on the internet
are fetched, over https with a valid certificate, and every redirect is checked
again, so an address cannot point the server at a machine in its own network.
The requests leave the way fetched mailboxes do: through the VPN when
`egress.fetch` is on ([configuration.md](configuration.md)).

**Read-only everywhere.** Nobody writes into a subscribed calendar but its
feed: CalDAV refuses to store or delete entries in it (and says so in the
calendar's WebDAV privileges, so apps show it read-only), JMAP says so in
`myRights`, and the portal will not import into it. Its name and colour stay
yours to change. It is left out of free-busy lookups (a holiday calendar does
not make anyone busy), never becomes the default calendar, and does not count
when invitations look for an event, so a subscribed copy of your own Google
calendar does not get in the way of the real events here.

**Reminders** of a feed ring on every device of everyone who subscribed, so
they are dropped unless *Keep reminders* is switched on.

**When the feed fails,** the calendar stays as it was, and the portal says what
went wrong. That includes a page that answers with anything but a calendar,
like a maintenance page: only a real calendar, even an empty one, changes what
is in it. After a failure the next try waits longer each time, up to a day.

**The address is a secret.** Google's secret address, for one, lets anyone who
has it read the calendar. It is kept sealed in the database, with the same key
as the passwords of fetched mailboxes ([fetch.md](fetch.md)), never shown
again after subscribing, and never written to the log: the portal and the log
show its host, like `calendar.google.com/…`. That key lives in the same
database, so this keeps the address out of an extract or a glance at a table,
not away from someone who holds the whole database or a backup.

*End subscription* either keeps the calendar with what it holds, as an
ordinary one you can change from then on, or deletes it. *Import once
instead*, in the same dialog, copies what the address holds now into a
calendar of your own, without subscribing.

Everyone can subscribe to 20 calendars; each is one of their 100 calendars and
address books.

## Moving from another provider

Your address there and a password is all it takes: the server finds the
provider's CalDAV and CardDAV servers, lists every calendar and address book
you have there and takes each over into a new one here, with its name and
colour. It asks, in this order, and stops at the first that answers:

1. a server you typed in yourself (*Enter the server myself*),
2. the servers of the providers it knows (below),
3. the domain's own `_caldavs._tcp` and `_carddavs._tcp` records with their
   `path=` (RFC 6764),
4. `https://<domain>/.well-known/caldav` and `/.well-known/carddav`.

A provider that says the password is wrong ends the search at once, so asking
elsewhere cannot fill its lockout counter. The password is used for this one
request and kept nowhere, not even in the browser once it is done. The login
only goes to the site it was given for: a server that redirects it to another
domain does not get it.

It all happens in one request, because some providers' app passwords work only
once. Calendars one merely subscribed to at the provider are feeds of their own
and are left out; subscribe to them here instead.

| Provider | What to know |
| --- | --- |
| iCloud | An app-specific password: appleid.apple.com → Sign-In and Security → App-Specific Passwords |
| WEB.DE | An app password, and each works only once: one for calendars, one for contacts, moved over one after the other |
| GMX | With two-factor authentication, an app password instead of the usual one |
| Posteo | The usual password, or an app password |
| mailbox.org, Fastmail | An app password from their settings |
| Nextcloud, ownCloud, Radicale, Baïkal, SOGo, … | Found through the domain, or type the server in; an app password where the server offers them |
| Google (Gmail) | Not this way: Google's CalDAV and CardDAV let in only apps registered with Google. Subscribe to each calendar by its *secret address in iCal format* (Google Calendar → settings of the calendar), or download it as a file; export contacts at contacts.google.com as a vCard file and import that |
| Outlook.com, Hotmail | No CalDAV or CardDAV at all: export the calendar as `.ics` and the contacts as a file there, and import them here |

Coming from mailcow, `uwumail-server import mailcow` takes calendars and
contacts along with everything else ([migrating-from-mailcow.md](migrating-from-mailcow.md)).

## Limits

| What | Limit |
| --- | --- |
| A file | 20 MB, 50 000 entries |
| An entry | 1 MB |
| A subscribed calendar | 16 MB, 20 000 entries per fetch |
| Subscribed calendars | 20 per person |
| Fetching again | every 15 minutes at most, once a week at least |
| Requests to other servers | 30 an hour per person (subscribing, fetching now, importing an address, moving) |
| Moving from a provider | 5 minutes, 64 MB per calendar or address book |
