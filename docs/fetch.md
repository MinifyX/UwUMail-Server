# Fetched mailboxes

Some addresses one cannot give up: a free mail address a service insisted on,
an old address from years ago that a few people still write to. This server can
empty those mailboxes into somebody's mailbox here, every few minutes, so their
mail arrives where all their other mail arrives — in every app, in the search,
in the backups — instead of in a second place nobody looks at.

Everyone sets their own up in the portal under *Mein Konto → Abrufkonten*, with
the address and the password for it. What the provider calls its servers, which
ports it listens on and how it spells the login is worked out by the server
itself, see [Finding the provider](#finding-the-provider). Mailboxes at
Microsoft and Google sign in there instead of handing this server a password,
see [Microsoft and Google](#microsoft-and-google).

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

## Microsoft and Google

Microsoft has switched plain passwords off for IMAP and SMTP at Outlook.com,
Hotmail and most Microsoft 365 tenants. A login is answered
`NO Basic authentication is disabled.`, app password or not, and nothing but
OAuth opens the mailbox any more. Google still takes app passwords, but only
with two-step verification switched on. So for both the dialog offers
**signing in there** first — *Mit Microsoft anmelden*, *Mit Google anmelden* —
and keeps the password as the way round for whoever wants it.

| Provider | Recognised by |
| --- | --- |
| Microsoft, personal accounts | `outlook.*`, `hotmail.*`, `live.*`, `msn.com`, `windowslive.com`, `passport.com` |
| Microsoft 365 | the domain's mail servers under `*.mail.protection.outlook.com` |
| Google | `gmail.com`, `googlemail.com`, and domains whose mail servers are Google's (Workspace) |

The domains are known at once; the mail servers are one DNS lookup, made by the
server once the address is typed. Microsoft's autodiscover is not asked.

After the sign-in the server **logs in for real** with what it got, over
SASL XOAUTH2, before anything is stored — as with a password. A sign-in with
another account than the address that was typed fails there. What the grant
opens is fixed per provider:

| | Incoming | Outgoing |
| --- | --- | --- |
| Outlook.com, Hotmail | `outlook.office365.com:993`, TLS | `smtp-mail.outlook.com:587`, STARTTLS |
| Microsoft 365 | `outlook.office365.com:993`, TLS | `smtp.office365.com:587`, STARTTLS |
| Google | `imap.gmail.com:993`, TLS | `smtp.gmail.com:465`, TLS |

### Microsoft works out of the box

Microsoft is signed in at with the device code flow (RFC 8628). The dialog shows
a short code in large letters, a button to copy it and the link to
[microsoft.com/devicelogin](https://microsoft.com/devicelogin); the person types
the code there and signs in with the mailbox's account. Meanwhile the server asks
Microsoft whether it is done — never more often than Microsoft asked to be
asked, and less often after a `slow_down` — and the page asks the server. Nothing
has to come back to this server's address, which may not even be reachable from
the internet.

UwUMail ships with the client ID of MinifyX's own Entra app
(`f4b09124-76e0-44a5-b675-2b35a898f0d7`), a public client without a secret for
personal accounts and every organisation, so there is nothing to set up. The
tenant is `consumers` for the personal domains above and `common` for everyone
else; the scopes are `https://outlook.office.com/IMAP.AccessAsUser.All`,
`https://outlook.office.com/SMTP.Send` and `offline_access`. A Microsoft 365
tenant whose admin allows no third-party apps asks for the admin's consent
first; that admin can grant it, or you register your own app.

**Your own Entra app**, if you would rather not rely on MinifyX's:

1. In the [Microsoft Entra admin center](https://entra.microsoft.com), *App
   registrations → New registration*. Supported account types: *Accounts in any
   organizational directory and personal Microsoft accounts*. No redirect URI.
2. *Authentication → Advanced settings → Allow public client flows*: **Yes**.
   The device code flow needs it; there is no secret.
3. *API permissions → Add a permission → Microsoft Graph → Delegated*:
   `IMAP.AccessAsUser.All`, `SMTP.Send`, `offline_access`, `email`, `openid`.
4. Copy the *Application (client) ID* into *Server → Einstellungen → Anmeldung →
   Abrufkonten: Microsoft und Google* (`fetch.oauth.microsoft_client_id`).

A grant belongs to the client it was made with: after changing the client ID,
mailboxes that signed in before have to sign in once more.

### Google needs your own client

Google has no device flow that covers mail, so it is the authorization code flow
with PKCE, and that needs a *Web application* client with a secret — one per
server, which only its admin can make:

1. In the [Google Cloud console](https://console.cloud.google.com), a project
   of its own.
2. *Google Auth Platform* (formerly the OAuth consent screen): audience
   **External**, an app name and a support address. Under *Data access*, add
   the scope `https://mail.google.com/`. Under *Audience*, **publish the app**
   to production: while it is *Testing*, Google ends every sign-in after seven
   days.
3. *Clients → Create client → Web application*, with this authorised redirect
   URI (the admin page shows it to copy):
   `https://<the server's public name>/api/account/fetch/oauth/callback`.
4. Client ID and secret into the same settings page
   (`fetch.oauth.google_client_id`, `fetch.oauth.google_client_secret`). The
   secret is sealed like the other secrets of the settings and never shown again.

`https://mail.google.com/` is one of Google's restricted scopes. Without Google's
verification — a paid security assessment, meant for products, not for a
server at home — people see *Google hasn't verified this app* and go on under
*Advanced*, and the app can be used by **at most 100 Google accounts** over its
lifetime. For a household or a club that is plenty.

The browser goes to Google and comes back to the redirect URI. The way back
carries no session (the session cookie is `SameSite=Strict`), so the sign-in is
tied to the browser that set off by a cookie of its own
(`__Host-uwumail-fetch-oauth`, ten minutes, `SameSite=Lax`), its `state` works
once, and what comes back is picked up only by the person whose session
started it. Google is asked with `access_type=offline` and `prompt=consent`, so
it hands out a refresh token every time.

### The tokens

What a sign-in leaves is a refresh token, sealed like a password (see
[The password](#the-password)), and short-lived access tokens made from it.
An access token is renewed five minutes before it runs out, one renewal at a
time per mailbox, and where the provider hands out a new refresh token with it
(Microsoft does), the new one is kept. Every request to the providers'
sign-in services leaves the way fetching does — through the proxy when
`egress.fetch` takes it, with the configured fallback — as HTTPS with a valid
certificate to public addresses only.

* **The provider ends the grant** (revoked, the password changed, a Google app
  still in *Testing*): the mailbox stops fetching and sending, its row says
  *Anmeldung abgelaufen – erneut anmelden* with a button for it, and its owner
  gets a notice in the inbox and in the security activity — once, not on every
  run. The provider is not asked again until somebody signs in anew.
* **The provider is down**: the next attempt waits a minute, then twice as
  long after every failure in a row, up to six hours.
* **Answering from the address** logs in to the provider's outgoing server with
  the access token. Without one the message waits in the queue; it never leaves
  another way, which the provider's DMARC policy would refuse.

### Mailboxes that still use a password

Microsoft's `Basic authentication is disabled` is told apart from a wrong
password: nothing counts towards a lockout and the search for other servers
stops. A new mailbox's dialog switches to *Mit Microsoft anmelden* by itself. An
existing one stops being asked on every run (*Jetzt abrufen* still tries), its
owner hears once that it has to be switched, and its row says *Microsoft lässt
keine Anmeldung mit Passwort mehr zu – bitte „Mit Microsoft anmelden“
verwenden* with *Auf Microsoft-Anmeldung umstellen*. Mailboxes at Microsoft and
Google that still work with a password are offered the same switch under
*Bearbeiten*. Switching keeps everything else — what was fetched, where the
folders stand, answering from the address — and forgets the password; typing a
password again later switches back.

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

* **The header block, always.** A message with more than one `From`, with
  `From` addresses in more than one domain, or with a header block a malformed
  line cut short is refused, as at the door: the person must see the one
  `From` the checks were made against.
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
* **New mail starts where the folders stood at the first run.** What was
  already there only comes when it is asked for — see
  [The mail that was already there](#the-mail-that-was-already-there).

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

## The mail that was already there

A mailbox that is set up mid-life already holds mail — often years of it. That
mail comes over when it is asked for: with *Vorhandene Mails übernehmen* when
the mailbox is added (on by default), or later with the clock button in its row.
From then on, next to the new mail, every run works through a portion of it —
200 messages per folder — and the next portion follows half a minute later
instead of after the mailbox's interval, until the folder holds nothing this
server has not seen.

It comes differently from new mail, on purpose:

* **With the date it had at the provider**, not the day it came over, and
  **read, unread or flagged as it was there**. A backlog is not a heap of new
  mail from today.
* **Where the provider had it, not judged again.** The inbox goes into the
  inbox and the junk folder into Junk, the way the migration import copies a
  mailbox. The spam filter would judge months-old mail against DKIM keys the
  senders have long since rotated and find it wanting; and since refused mail
  is cleared at the provider, a mailbox set to delete would lose good old mail
  that way. The virus scanner is not asked either. This is the person's own
  mail, from a mailbox they proved is theirs by opening it.
* **Only what is not here yet.** A message that is already in the mailbox —
  fetched before, imported, or sent here directly as well — is recognised by
  its `Message-ID` (by its bytes where it has none) and not brought twice. That
  is what makes it safe to ask again, and to ask for it on a mailbox that has
  been fetching for a while.

Afterwards it is marked as read or deleted at the provider like any other
message. A full mailbox here stops it the way it stops new mail: it waits at
the provider, untouched, and carries on once there is room.

It goes only into a mailbox of the person's own. A service account whose mail
is redirected elsewhere does not get its old mail sent on: that stays at the
provider.

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

A sign-in at Microsoft or Google keeps no password at all: its refresh and
access tokens are sealed the same way instead.

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
| Messages per folder and run | 200, and 200 more of the mail that was already there |
| While the mail that was already there comes | the next run follows after 30 seconds |
| How long a run may take | 5 minutes |
| A message waiting to be taken | stepped over after 24 hours |
| Message names remembered | 30 days |
| Sign-ins on their way | 3 per person; Microsoft's code is good for 15 minutes, Google's way back for 10 |

## Known limitations (security review)

- Messages are fetched by `RFC822.SIZE` within the 512 MiB budget all imports share; the size answers and the copies made while storing a message are outside it.
- A message larger than a fetch takes is left at the provider and stepped over; its UID is only in the server log.
- Fetched mailboxes have no crash counter like moves do: a message that brings the server down would be fetched again after the restart.

## What is not there yet

* **STARTTLS on port 143.** Fetching is over TLS from the first byte, which is
  what every provider worth using offers on port 993.
* **IDLE.** A run happens on its interval; the provider is not asked to keep a
  connection open and announce new mail.
* **Signing in at other providers.** Only Microsoft and Google; Yahoo and AOL
  still take app passwords.
