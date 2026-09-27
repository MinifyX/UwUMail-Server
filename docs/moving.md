# Moving from another provider

Changing to your own server should not mean leaving years of mail behind.
Under **My account → Moving** everyone can bring the mail of an old mailbox
over by themselves: the old address and its password are all it takes, and the
server copies everything in the background.

Calendars and contacts move separately, under *My account → Calendars &
contacts → Bring them over* ([calendar-import.md](calendar-import.md)). A whole
server with all its people moves on the command line instead
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

A move pauses, and says why, when the person has to do something:

| Reason | What to do |
| --- | --- |
| The mailbox here is full | Make room or ask the admin for more, then *Continue*. What came so far stays. |
| The old provider refused the password | Enter a new one (often an app password) and *Continue*. |
| The provider could not be reached, or something else went wrong | *Continue* tries again. |
| Paused by hand | *Continue* whenever you like. |

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

## For admins

The moves are kept in the table `migration_jobs` (migration 0040); where each
folder got is in `import_progress` under `move:<login>@<host>`. The worker runs
inside the server; there is nothing to set up. `uwumail-server import imap`
remains the way to move many mailboxes at once with a master user.
