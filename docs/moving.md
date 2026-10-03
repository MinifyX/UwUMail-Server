# Moving from another provider

Changing to your own server should not mean leaving years of mail behind.
Under **My account → Moving** everyone can bring the mail of an old mailbox
over by themselves: the old address and its password are all it takes, and the
server copies everything in the background.

Calendars and contacts move separately, under *My account → Calendars &
contacts → Bring them over* ([calendar-import.md](calendar-import.md)). A
whole domain with all its people, or a single mailbox, is moved by an admin under
**Admin → People → Moves** ([below](#moving-a-domain-admins)); a whole server
with a master user still moves on the command line
([migrating-from-mailcow.md](migrating-from-mailcow.md)).

## Starting a move

1. Enter the **old address** and its **password**. Many providers want an app
   password here rather than the one you sign in with; the page says so for the
   ones it knows (see [Providers](#providers)).
2. The server finds the provider's IMAP server the same way fetched mailboxes do
   ([fetch.md](fetch.md#finding-the-provider)): the domain's own records, the
   provider's autoconfig file, Mozilla's database, and a guess, and it logs in
   once before anything is kept. When nothing answers, *Enter the server myself*
   takes the IMAP server, port and user name directly.
3. The move is queued and starts within seconds. The page shows how far it got
   and follows it on its own; it can be closed, the copy goes on.

## What comes along

- **Every folder** of the person's own namespace, with its place in the tree.
  Shared and public folders of the old provider stay there.
- The special folders go into ours: the inbox into the inbox, and sent, drafts,
  junk, trash and archive (by their IMAP special-use flags, or their usual
  English and German names) into the ones this server has.
- Every message with the **date it arrived** there, and **read, flagged,
  answered** and other keywords as it was. Messages marked as deleted stay
  behind.
- **Nothing twice.** A message this mailbox holds already, by its Message-ID or,
  without one, by its content, is left out and counted as such. That covers mail
  that came here some other way already, and providers that show one message in
  several folders: at Gmail every label is a folder, and a message with two
  labels comes once, into the first folder it is found in (the inbox first).

## How it runs

One move is copied at a time, for up to five minutes, then the next one gets its
turn, so one big mailbox does not hold up everybody else's. Each folder
remembers the last message it took over (by the folder's IMAP UIDs), so a turn
that ended, a restart of the server or a broken connection all go on where
things stood. A provider that renumbered a folder since is copied again from
the start, and the duplicate check keeps it from bringing anything twice.

A turn has ten minutes in all, connecting and logging in included: a provider
that takes the connection and then does not answer, or never finishes answering
the login, pauses the move as unreachable instead of holding up everybody
else's. Each command to the provider has five minutes to be answered (half an
hour for a portion of messages).

Messages are fetched by size: the provider is first asked how large they are
(`RFC822.SIZE`), small ones come in portions of about 16 MiB, large ones one at
a time, and a fetch may take only a little more than the size the provider gave.
A message larger than this server takes (`smtp.max_message_size`) is left out
and counted with the skipped ones. All imports together (admin moves, personal
moves, fetched mailboxes) hold at most 512 MiB of fetched mail at once; the
rest waits for its turn.

When the server starts and finds a move still running, it went down during it
(a clean stop puts running moves back in the queue). The move goes on, but the
third start in a row that finds it running pauses it as `interrupted`, so a
mailbox that brings the server down cannot do so again and again.

A move pauses, and says why, when the person has to do something:

| Reason | What to do |
| --- | --- |
| The mailbox here is full | Make room or ask the admin for more, then *Continue*. What came so far stays. |
| The old provider refused the password | Enter a new one (often an app password) and *Continue*. |
| The provider could not be reached, or something else went wrong | *Continue* tries again. |
| Paused by hand | *Continue* whenever you like. |
| The server stopped during the move several times in a row | Ask the admin to look at the server log, then *Continue*. |

Nothing is retried behind the person's back, so a wrong password never runs
into the provider's lockout.

When everything is here the move is **done**. *Sync again* fetches only what
arrived at the old provider since, as often as you like: move once now, tell
people the new address, and sync again a week later for the stragglers.
*Done, delete login* ends the move for good and deletes the password; the mail
that came stays.

## Providers

| Provider | What it wants |
| --- | --- |
| Gmail | An app password: turn on 2-step verification in the Google account, then create one at myaccount.google.com/apppasswords. IMAP is always on. |
| Outlook.com, Hotmail | Only apps that sign in through Microsoft (OAuth) get in, which a server of your own cannot do for you. Copy the mail with a mail program instead: add both mailboxes and drag the folders over. |
| GMX, WEB.DE | Access over IMAP has to be switched on in their settings (*E-Mail → POP3/IMAP Abruf*); with two-factor authentication, an app password. |
| iCloud | An app-specific password from appleid.apple.com. |

## Security

- The password is sealed in the database with the same key as the passwords of
  fetched mailboxes (AES-256-GCM), is never shown again, and goes with the move.
- The server connects only over TLS (port 993) with a certificate valid for the
  name, and only to public addresses: the name is checked again before every
  turn, so it cannot come to point at this machine or the local network. When
  the admin sends fetching through the egress proxy (*Server → Settings →
  VPN & proxy*, `egress.fetch`), moves take it too.
- Starting a move and going on with one log in at the provider, so both are
  limited to ten an hour per person. At most five moves can exist at once, and
  an address of this server cannot be moved into itself.
- Answers of the old provider are size-capped like those of the command-line
  import, so a hostile server cannot fill the memory.

## Moving a domain (admins)

Under **Admin → People → Moves** an admin moves a whole domain with everyone on
it, or one mailbox, from another server: mail, contacts and calendars. The same
copying code as *My account → Moving* does the work, so everything under
[What comes along](#what-comes-along) holds here too.

### The wizard

1. **What moves.** *Domain move*: everyone of a domain. The domain is made if it
   is not here yet, with DKIM keys (check its DNS afterwards under *Domains*).
   *Single mailbox*: one old mailbox into an existing mailbox here (its mail
   stays, folders are merged, nothing comes twice) or into a new one.
2. **Old server.** One IMAP server (TLS, port 993) for the whole move; *Find
   server* looks it up like a fetched mailbox does. Best is a name that keeps
   pointing to the old server after the MX switch. Contacts and calendars come
   over CalDAV/CardDAV with the same login: *Find automatically* (known
   providers, the domain's records, `/.well-known/caldav` and `carddav`, then
   the IMAP server), or a preset: mailcow/SOGo (`https://host/SOGo/dav/`),
   Nextcloud (`https://host/remote.php/dav/`), iCloud, GMX, WEB.DE, an own
   address, or *Files only*. Every row can name its own IMAP server and DAV
   address.
3. **People.** One row per person: old address, old login (empty = the old
   address), old password, display name, address here (empty = the old local
   part on this domain), quota and aliases (aliases do not come over IMAP).
   The table fills from a CSV list too, see below.
4. **Check and start.** The check changes nothing and names every problem by
   row and field (address taken, alias on a foreign domain, mailbox already in
   another move and so on). *Start move* makes what is missing: the domain,
   mailboxes **without a password** (their people get a link), and aliases.
   Name and quota are only set on mailboxes the move makes; existing ones keep
   theirs.

Starting the same list again is harmless: existing mailboxes are filled, not
made twice, and a mailbox can be in only one open move.

### CSV lists

Paste or upload what a spreadsheet exports (at most 1 MiB, 2000 rows). The
delimiter is found by itself: `;`, `,` or a tab; quotes work as usual. A header
line is recognised by its names, in English or German:

| Column | Header names |
| --- | --- |
| Old address (required) | `old address`, `address`, `email`, `alte adresse`, `adresse` |
| Old password (required) | `password`, `passwort`, `kennwort` |
| Display name | `name`, `display name`, `anzeigename` |
| Address here | `new address`, `target`, `neue adresse`, `ziel` |
| Quota | `quota`, `kontingent` (`2 GB`, `500 MB`, `1,5G`; a bare number is MB) |
| Aliases | `aliases`, `aliase` (separated by spaces, commas or `\|`) |
| Old login | `login`, `user`, `benutzer` |

Without a header the columns go in this order: old address; password; name;
address here; quota; aliases; login. Lines that cannot be read are listed with
their line number and what is wrong; the good ones go into the table.

```
Alte Adresse;Passwort;Name;Neue Adresse;Quota;Aliase
mini@example.com;geheim;Mini Muster;;2 GB;info@example.com
nyu@example.com;nyan;Nyu;nyu.neko@example.com;;
```

### Progress and control

The page of a move shows how far it got overall and per mailbox: folders,
messages (and how many were here already), contacts and calendar entries.
Each mailbox has its own state: *waiting*, *copying*, *paused*, *up to date*
and *finished*.

- **Pace.** *Mailboxes at once* (1–8, default 2) limits how many mailboxes of
  this move are copied at the same time, so the old server is not overrun; the
  whole server copies at most four mailboxes of all moves at once, each for
  up to five minutes per turn. *Minutes between rounds* (5–1440, default 60)
  sets how often new mail is fetched once a mailbox is up to date.
- **Errors** pause only the mailbox concerned and say why (old server refused
  the login, unreachable, not on the internet, mailbox here full). *Retry*
  goes on where it stopped and can take a new login or password; nothing is
  retried behind the admin's back, so a wrong password does not run into the
  provider's lockout.
- **Pause / Continue** for the whole move or one mailbox; *Take out of the
  move* forgets that mailbox's old password.
- **Restarts.** Every folder remembers how far it got; after a restart of the
  server the moves go on by themselves.
- **Quota warning.** When the old server says how big the old mailbox is and it
  does not fit into the quota here, the row says so before it runs full.
- Contacts and calendars come in the first complete round and again in the
  last one; the collections found then are kept, so a DNS change after the MX
  switch does not send them elsewhere. When they cannot be fetched, the row
  says why and offers an upload of `.vcf` and `.ics` files instead. Contact and
  calendar folders on the IMAP server (Kolab style: top-level folders named
  Contacts/Kontakte/Adressbuch or Calendar/Kalender holding vCards or
  iCalendar parts) are imported into address books and calendars as well, for
  the kinds the move takes (contacts, calendars) only. Only messages that are
  nothing but such an object (the vCard or iCalendar part, at most a short note
  or Kolab's own XML next to it) become contacts or events; an email filed
  there (an invitation, recognised by its iTIP `METHOD` even with a short note,
  an HTML body or other attachments) is copied as mail, and so is every object
  whose import fails, so no mail is lost.

### Finishing after the MX switch

Until the move is finished, every mailbox is synced again and again, so mail
that still arrives at the old server keeps coming over. Point the domain's MX
records to this server, *Check MX* shows whether they do, wait until mail
arrives here, then **Finish move**: every mailbox gets one last round, then the
old passwords are wiped and the move is done. *Finish without last round* wipes
them at once, for an old server that is gone already.

### Password links

The mailboxes a move made have no password. *Make links* creates a link per
mailbox to choose one (the usual invite links, valid for 7 days), shown once:
copy them one by one, download them as a CSV file (`;`, UTF-8 with BOM, opens
in spreadsheets) or print an overview to hand out. *Make new links* replaces
the old ones. Mailboxes whose people have a password already get no link and
the list says so, as it does for disabled accounts and ones that sign in at the
directory.

### Security

- Only admins see and use moves; every step lands in the audit log
  (`move.create`, `move.finish`, `move.retry` and so on, plus the accounts,
  aliases, domain and password links the move made).
- The old passwords are sealed in the database (AES-256-GCM, the key of fetched
  mailboxes) until the move is finished or the mailbox is taken out, and are
  never shown again.
- Old servers must be on the internet, the same rule as fetched mailboxes and
  personal moves; servers in the local network go through
  `uwumail-server import imap` instead. CalDAV/CardDAV addresses must be
  `https` without a user name in them.
- At most 20 moves can be open at once, 2000 mailboxes per move; server lookups
  are limited to 30 an hour per admin; uploads to 20 MiB. These limits and the
  mailboxes that are busy are checked before the move makes a domain, mailbox
  or alias; when the move is refused after all, what it made on the way is
  taken back (audited with `"undone": true`).
- A mailbox is filled by one move at a time: an admin's move refuses a mailbox
  whose person moves mail in themselves (`movePersonalBusy`), and a person
  cannot start or resume their own move while an admin's move fills their
  mailbox (`moveAdminBusy`).
- When an account goes to the trash, its open move entries stop
  (`accountDeleted`) and their old passwords are wiped; after a restore, retry
  with the password.
- The links CSV neutralises cells a spreadsheet would read as a formula
  (starting with `=`, `+`, `-`, `@`, tab or CR get a leading `'`).
- Messages are fetched by size, larger than `smtp.max_message_size` are
  skipped, and all imports share a 512 MiB budget of fetched mail; a mailbox
  the server went down during at three starts in a row is paused as
  `interrupted` (see *How it runs*).

### Known limitations (security review)

- `…/import` (upload `.ics`/`.vcf`) still works on a finished move and on an account in the trash.
- There is no limit on the number or the depth of the folders a move copies.
- DAV import merges into a collection of the same name and can overwrite items with the same UID when it fills a mailbox that already has them.
- Imported events keep their alarms (`VALARM`).

### API

All under `/api/admin/moves`, for admins only (403 otherwise):

| Method and path | What it does |
| --- | --- |
| `GET /` | the moves with their totals and the limits |
| `POST /` | start a move (`dryRun: true` only checks; row problems come back as 409 `moveRows` with `blockers`) |
| `POST /discover`, `POST /csv` | find the old server of an address; read a CSV text into rows |
| `GET /{id}`, `PATCH /{id}`, `DELETE /{id}` | a move with its mailboxes; change its pace; delete it (old passwords go with it) |
| `POST /{id}/pause`, `/resume`, `/finish` | pause, continue, finish (`skipLastRound`) |
| `POST /{id}/mx`, `POST /{id}/links` | check the domain's MX; make password links |
| `POST /{id}/mailboxes` | add people to a domain move |
| `POST /{id}/mailboxes/{mailbox}/retry`, `/pause`, `/import?kind=calendar\|addressbook`; `DELETE /{id}/mailboxes/{mailbox}` | per mailbox: retry (optionally with a new login or password), pause, upload `.ics`/`.vcf`, take out |

## For admins

Personal moves are kept in the table `migration_jobs` (migration 0040), admin
moves in `moves` and `move_mailboxes`; where each folder got is in
`import_progress` under `move:<login>@<host>` for both, so a personal move and
an admin move of the same old mailbox go on from each other. The worker runs
inside the server; there is nothing to set up. `uwumail-server import imap`
remains the way to move many mailboxes at once with a master user.
