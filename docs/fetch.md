# Fetched mailboxes

Some addresses one cannot give up: a free mail address a service insisted on,
an old address from years ago that a few people still write to. This server can
empty those mailboxes into somebody's mailbox here, every few minutes, so their
mail arrives where all their other mail arrives — in every app, in the search,
in the backups — instead of in a second place nobody looks at.

Everyone sets their own up in the portal under *Mein Konto → Abrufkonten*, with
the address and the password for it. What the provider calls its servers, which
ports it listens on and how it spells the login is worked out by the server
itself, see [Finding the provider](#finding-the-provider).

## Finding the provider

Almost nobody knows that iCloud keeps its mail under `imap.mail.me.com`, or
that it wants only the part before the `@` as the login while its own outgoing
server wants the whole address. So the address and the password are all that is
asked for, and the server finds the rest. It asks in this order and stops at the
first source that answers:

| Source | What it is | Who is found there |
| --- | --- | --- |
| The domain itself | the `_imaps._tcp` and `_submission(s)._tcp` records of RFC 6186 | mail.de, iCloud |
| The provider's own file | Thunderbird's autoconfig, at `autoconfig.<domain>` and under `.well-known` on the domain | providers that publish one |
| Mozilla's database | the collection Thunderbird ships with | GMX, web.de, t-online |
| Guessing | `imap.<domain>`, `mail.<domain>`, the usual ports | anyone else |

None of it is taken on trust: the server **logs in for real** before a mailbox
is stored, so what is saved is what a connection answered to, not what a
database claims. The one thing no source states reliably — whether the login is
the whole address or only the part before the `@` — is settled the same way:
when the whole address is refused, the local part is tried once, and never more
than that, so this cannot walk into a provider's lockout. A provider that
answers and says the password is wrong ends the search then and there, because
asking the next candidate with the same wrong password only fills its counter.

The outgoing server is proven separately, with the same password, and is left
out when it does not answer — a mailbox that cannot send is better than one that
claims it can.

Whoever has a provider that none of this finds types the names in themselves,
under *Server selbst eintragen* in the same dialog; that is also what the dialog
opens by itself when the search came back with nothing.

Two things this reaches out to the internet for: the provider's file, and
Mozilla's database, which learns the domain of the address being set up. Both go
through the same door as the subscribed word lists — HTTPS with a valid
certificate, public addresses only — and so do the logins, so an address nobody
has proven yet cannot point this server at its own network.

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
* **Nothing is touched at the provider before this server has decided.** A
  message is marked or deleted there only once it has either arrived here or
  been refused here for good. An answer of "later" — greylisting asking the
  sender to come back, a full mailbox — is not a decision: it leaves the message
  where it is and stops the folder at it, so the next run offers it again.
  That is exactly what greylisting asks of a sending server, and here this
  server is that sender. A message that cannot be taken for a whole day is
  stepped over, so one message can never block a folder for good.
* **The first run takes nothing.** It only writes down where the folders stand.
  Years of old mail would otherwise arrive as if it came today. To bring the
  existing mail over with its folders and its dates, use the migration import:
  `uwumail-server import imap`, see [migrating-from-mailcow.md](migrating-from-mailcow.md).

A run takes at most 200 messages per folder and is cut short after five
minutes; what is left waits for the next one.

**Refused mail is cleared at the provider too.** A message the filter turns
away — a virus, a blocked sender, a DMARC policy that rejects, a score over the
limit — is not brought here, and at the provider it is dealt with exactly like
one that arrived: marked as read, or deleted, by what the mailbox is set to.
This server has made its decision, and a fetched mailbox that keeps everything
it refuses is one that fills up and that nobody ever empties.

What stays behind is the record, not the message: the history under
*Spamfilter* keeps who sent it, its subject and why it was refused — for 30 days
by default, and not at all where the history is switched off. With *Endgültig
löschen* the message itself is then gone for good; with *Als gelesen
markieren* it is still at the provider, just no longer unread. Whoever wants a
second look at refused mail sets the mailbox to mark as read.

The one exception is mail this server has nowhere to put: when the mailbox it
fetches into is gone, or its address takes no mail. That is not a verdict on
the message but a mistake on this side, and somebody's mail is not deleted over
it — it stays at the provider, untouched, and is stepped over so the folder
does not stick on it.

## At the provider afterwards

| Setting | What happens |
| --- | --- |
| Als gelesen markieren | the message is flagged `\Seen` and stays. Nothing is lost if a run goes wrong. This is the default. |
| Endgültig löschen | the message is deleted there. Good for a mailbox one only keeps because a service insists on it. |

## Answering from a fetched address

A reply from a free mail address has to leave through that provider's own
outgoing server. Sent from here it would carry our name on the envelope while
claiming theirs in the `From` header, and their DMARC policy would take it
apart at the recipient — the very policy that makes the address worth
something.

So a fetched mailbox can learn where its provider takes outgoing mail, under
*Von dieser Adresse antworten* on the same page. The server is found with
everything else and proven with the same password before it is offered; where
nothing was found it is typed in by hand, as STARTTLS on port 587 or TLS on
port 465. Never unencrypted: this sends a password across the internet. The
password is the one that is already stored for fetching — providers use the same
one for both, and a second one to keep in sync would only be a second one to get
wrong.

Two things follow from that:

* **The address may be sent from only by the person who fetches it.** Two
  people can fetch the same provider, and neither may send as the other's. The
  one place that decides who may send as which address asks for the account,
  not only for the address.
* **Without a server there is nothing to switch on.** Sending cannot be
  enabled until an outgoing server is set, so no address ever claims it can
  answer when it cannot.
* **One successful fetch has to come first.** Until this server has really
  emptied the mailbox once, sending from the address stays off, whatever the
  row says. A row anyone can write is not proof that the mailbox is theirs;
  opening it is. So a mailbox that was just set up carries its outgoing server
  from the start and gets the switch after its first run — the dialog for a new
  mailbox says so instead of offering a switch that would be refused.

Mail from such an address then goes out through the provider whoever it is
addressed to — before the server's own smarthost, if one is configured, because
whose address it comes from decides where it may leave.

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
* **IDLE.** A run happens on its interval; the provider is not asked to keep a
  connection open and announce new mail.
