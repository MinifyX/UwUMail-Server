# Fetched mailboxes

Some addresses one cannot give up: a free mail address a service insisted on,
an old address from years ago that a few people still write to. This server can
empty those mailboxes into somebody's mailbox here, every few minutes, so their
mail arrives where all their other mail arrives — in every app, in the search,
in the backups — instead of in a second place nobody looks at.

Everyone sets their own up in the portal under *Mein Konto → Abrufkonten*, with
the address, the provider's IMAP server and the password for it.

## What happens to fetched mail

It goes through the same pipeline as mail another server hands in at the door:
the same checks, the same spam filter, the same allowed and blocked senders,
the same forwarding and away messages, the same history under *Server →
Spamfilter*. It lands in the inbox or in Junk by what this server thinks of it.

The provider's own junk folder is emptied too, unless that is switched off.
Mail from it is not filed as spam because the provider said so: it is judged
again from scratch, and the provider's verdict is one rule among many. That
way a newsletter the provider got wrong still reaches the inbox, and spam the
provider missed does not get a free pass either.

## What the filter can still ask

A fetched message has already been delivered once. The connection it came in
on was the provider's, not the sender's, and the envelope is gone. Every rule
that judges a sending server — its address, its reverse name, the blocklists,
SPF — has nothing left to look at. Instead of guessing, the filter asks what it
still can:

* **DKIM, checked here.** A signature travels with the message, so it is worth
  exactly as much as it was before: it says who signed the message, and when it
  holds for the From domain, that is what DMARC alignment asks of DKIM.
* **What the provider found out**, from the `Authentication-Results` header it
  wrote. That header counts only when it carries the provider's own name (the
  registrable domain of its IMAP server, or a name set by hand). Only the
  topmost one is read: a provider writes its header above everything the
  message brought along, so anything below it may be the sender's own work.
* **The address the provider saw.** When the provider named one under its own
  name, this server checks that address itself — and from there on the message
  is judged exactly like one handed in at the door, blocklists and all.
* **Everything else only against a message, never for it.** A `Received-SPF`
  line nobody signed for, a spam flag in the headers, the junk folder it lay
  in: each of these can add points, none of them can take any away. Forging
  them only hurts the forger.

### The rules a fetched message answers to

| Rule | Points | When |
| --- | --- | --- |
| `FETCHED` | 0 | always, naming the mailbox it came from — so the headers and the history say that this one was fetched |
| `PROVIDER_JUNK` | +2.5 | it lay in the provider's junk folder |
| `PROVIDER_SPAM_FLAG` | +2.0 | the provider marked it as spam in the headers (`X-Spam-Flag` and the like) |
| `PROVIDER_SPF_FAIL` | +2.0 | SPF failed, by the provider's own header or an unsigned `Received-SPF` |
| `PROVIDER_DKIM_FAIL` | +1.0 | the provider says a signature did not hold |
| `PROVIDER_DMARC_FAIL` | +2.5 | the provider says DMARC failed |
| `FETCHED_NO_AUTH` | +1.0 | nothing vouches for the message: no signature held here, and the provider says nothing |
| `NOT_ADDRESSED` | +0.5 | the fetched address is in neither `To` nor `Cc` |

The three authentication rules only count when this server could not check the
sending address itself; when it could, SPF, DKIM and DMARC are scored the usual
way and nothing is counted twice.

The provider's verdict alone — junk folder and spam flag together, 4.5 points —
stays under the 5.0 that Junk starts at. It takes the message's own doing to
get there. See [spam-filter.md](spam-filter.md) for everything else that
scores.

## How a run works

Every 30 seconds the server looks at which mailboxes are due; each one has its
own interval, five minutes by default. A run connects over TLS (port 993),
takes the inbox and the junk folder, and walks the UIDs it has not seen.

Three things it is careful about:

* **A message is taken once.** Each folder remembers the last UID it read. A
  provider that renumbers a folder (a new `UIDVALIDITY`) starts the count over,
  and then the message's own `Message-ID` keeps it from arriving twice. Names
  are remembered for 30 days.
* **A message is never lost.** Nothing is marked or deleted at the provider
  until this server really has the message. An answer of "later" — greylisting
  asking the sender to come back, a full mailbox — leaves it where it is and
  stops the folder at it, so the next run offers it again. That is exactly what
  greylisting asks of a sending server, and here this server is that sender. A
  message that cannot be taken for a whole day is stepped over, so one message
  can never block a folder for good.
* **The first run takes nothing.** It only writes down where the folders stand.
  Years of old mail would otherwise arrive as if it came today. To bring the
  existing mail over with its folders and its dates, use the migration import:
  `uwumail-server import imap`, see [migrating-from-mailcow.md](migrating-from-mailcow.md).

A run takes at most 200 messages per folder and is cut short after five
minutes; what is left waits for the next one. Mail that is refused — a virus, a
blocked sender, a DMARC policy that rejects — is not brought here, and it is
not deleted at the provider either, whatever the mailbox is set to: whoever
wants to see it can still find it there.

## At the provider afterwards

| Setting | What happens |
| --- | --- |
| Als gelesen markieren | the message is flagged `\Seen` and stays. Nothing is lost if a run goes wrong. This is the default. |
| Endgültig löschen | the message is deleted there. Good for a mailbox one only keeps because a service insists on it. |

## The password

The password belongs to the provider, so unlike the passwords here it cannot be
hashed — the server has to send it to log in. It is sealed with AES-256-GCM
under a key of this server and never comes back out: no page and no endpoint
shows it, and an update that leaves it out keeps the stored one.

The key sits in the same database. That keeps the password out of an extract,
a log line or a glance at the table; it is not a defence against somebody who
holds the whole database. Treat a fetch account's password the way you would
treat the mailbox it opens, and prefer an app password where the provider
offers one — iCloud and Google require one anyway, and it can be revoked on its
own.

## Limits

| | |
| --- | --- |
| Mailboxes per person | 10 |
| How often | every 60 seconds to every 6 hours, 5 minutes by default |
| Messages per folder and run | 200 |
| How long a run may take | 5 minutes |
| A message waiting to be taken | stepped over after 24 hours |
| Message names remembered | 30 days |

## What is not there yet

* **STARTTLS on port 143.** Fetching is over TLS from the first byte, which is
  what every provider worth using offers on port 993.
* **Sending as the fetched address.** Replying from it needs the provider's own
  outgoing server, otherwise its DMARC policy takes the reply apart.
* **IDLE.** A run happens on its interval; the provider is not asked to keep a
  connection open and announce new mail.
