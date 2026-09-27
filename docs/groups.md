# Groups and shared mailboxes

Two ways for several people to use one address:

- A **group** (`vorstand@`, `info@` of a club) has no mailbox. Mail to it goes
  to each member's own mailbox, as if it had been sent to them.
- A **shared mailbox** (`support@`) is a mailbox of its own that several people
  use together. Mail to it stays in it. Its members see all of its folders next
  to their own and can answer as it. An existing person or service can be turned
  into one.

Admins set both up. People see what they belong to under
*My account → Addresses → Groups and shared mailboxes*.

Masked addresses, the third kind of address from the same release, are in
[jmap-masked-email.md](jmap-masked-email.md).

## Groups

*Accounts & domains → a domain → Groups → New group* takes:

| Field | |
| --- | --- |
| Address | the part before the `@`; the domain is the one the page is for |
| Name | shown in the list and used as the name of the members' sending identity |
| Who may write to it | `anyone`, only its `members`, or only addresses of the group's `domain` |
| Members may send as the group | the members may use its address as their From |
| Members | people (and services) of this server, up to 500 |

A group's address is an address like any other. It cannot be taken by a
person, an alias, a forwarding address, a masked address or another group,
and a domain with a group cannot be removed. `vorstand+sitzung@` reaches the
group too. Removing a group frees the address; the catch-all takes its mail
again, if there is one.

### Delivery

Mail to a group is delivered to every member the way mail to their own address
is:

- Each member's own **spam decision** counts: their sender lists, their
  Bayes filter and their own limits can send their copy to Junk while the
  others get it in the Inbox.
- Each member's **Sieve rules** sort their copy ([sieve.md](sieve.md)); the
  envelope recipient the rules see is the group's address.
- Each member's **forwarding** passes their copy on.
- A member reached twice — directly and through the group, or through two
  groups — gets the message once. A member who writes to the group gets a
  copy too, like everyone else in it.
- A member whose mailbox is full misses this message, and nobody else does.
  Only when no member could take it does the sender hear `552 5.2.2`, as for
  one full mailbox. A group without members, or whose members all have no
  mailbox, refuses mail at RCPT with `550 5.1.1`.
- A member who is a service without a mailbox hands the message to the address
  it passes its mail to.
- A message that already carries `Delivered-To:` with the group's address went
  through it before, for example when a member forwards to an address that
  forwards back to the group. It is taken, but not delivered again.

Vacation replies answer only mail sent to one's own addresses, so mail to a
group does not trigger them.

### Who may write to it

- **Anyone:** every sender, as for any other address.
- **Members:** the envelope sender (`MAIL FROM`) must be one of the members'
  own addresses (a `+tag` is fine), or the message comes from a member who is
  logged in to submission, JMAP or the webmail.
- **Domain:** the envelope sender must be an address of the group's domain.

Everyone else is refused at RCPT with `550 5.7.1`. Mail from other servers
must also be verified for the two closed kinds: SPF has to pass for the
envelope sender's domain, or a DKIM signature of that domain has to hold.
Otherwise anybody could claim a member's address; such a message is refused
after DATA with `550 5.7.1`. A member who writes from their own mail app is
logged in and needs neither.

On submission a sender who may not write to the group gets a bounce for that
recipient; the other recipients are not affected.

### Sending as the group

With *Members may send as the group*, each member may use the group's address
as From and envelope sender (`+tag` included) over SMTP submission, JMAP and
the webmail. They find it among their JMAP identities, named after the group.
The identity comes with the membership and goes with it, and also when the
switch is turned off. Replies to such a message go to the group, so every
member sees them.

### Principals

Over JMAP (`urn:ietf:params:jmap:principals`), `Principal/get` and
`Principal/query` list each group as a principal of `type: "group"`, with the
id `g<number>`, the group's name and address, and `accounts: null`. A group is
nobody one can share a folder with; `shareWith` takes people only.

## Shared mailboxes

*Accounts & domains → Accounts → Shared mailboxes → New shared mailbox* takes a
name, an address, a storage limit and the members. For each member a switch
says whether they may **send** with its address.

A shared mailbox is a **service with members** (the portal marks it *Shared
mailbox*). It has its folders, its storage limit and its addresses, and admins
can add aliases to it like to anyone else's account. Nobody signs in to the
portal or the webmail as it: it has no password and cannot get one. Its members
use it with their own logins. Only people can be members; a service or another
shared mailbox cannot, and a shared mailbox is never a member of itself.

Like any service it has **app passwords** and **protocol switches**, on its
page in the portal. A scanner, a shop or a mail app set up with its own address
logs in with one of them over SMTP, IMAP, JMAP or CalDAV/CardDAV, as far as its
switches allow; the members see the same mail. IMAP or JMAP has to stay on,
since without them no mail is kept for it (`sharedMailboxNeedsMailbox`). What
it sends itself this way is filed by the mail app, as for anyone.

A shared mailbox may be a member of a [group](#groups): mail to the group then
lands once in the shared mailbox, where all its members see it.

### Turning an account into one

An existing person or service becomes a shared mailbox with *Turn into a shared
mailbox* on its page (not your own), which asks for the members right away. The
mail, the folders, the addresses and the storage limit stay.

- **A person** becomes a service on the way, as in
  [configuration.md](configuration.md#accounts-people-and-services): their
  password turns into an app password that does not expire, so their mail apps
  keep working, and their second factors, passkeys, sessions, password links,
  apps signed in with OAuth and the tie to an LDAP directory or OpenID Connect
  provider go. What they set up for their own mail stops as well, so that
  nothing of the mailbox keeps going to them: their forwarding (a copy stays
  here again), the mailboxes they fetched from other providers and moves from
  another provider (with the passwords for those), updates of subscribed
  calendars (the calendars stay), and their active mail rule (the scripts
  stay, switched off). Their masked addresses are switched off; they stay with
  the mailbox and can be switched on again. Send-as domains an admin gave the
  account stay.
- **A service** keeps its app passwords. One without a mailbox (sending only)
  gets IMAP and JMAP switched on and its folders.
- Either way it leaves what it used as a person: folders others shared with it
  and the shared mailboxes it was a member of, with the sending addresses those
  gave. Its own folders stay shared as they were, and its groups stay.

*Make it a plain service* on the page of a shared mailbox is the way back: the
members lose its folders and its sending addresses, and the mail, the addresses
and the app passwords stay. From there it can become a person again the usual
way.

The same from the terminal:

```bash
docker compose exec uwumail uwumail-server account shared info@example.com on \
  --member mini@example.com --sender leni@example.com
docker compose exec uwumail uwumail-server account shared info@example.com off
```

`--member` gives a member who reaches every folder, `--sender` one who may also
send with its addresses; both may be given more than once.

### Its folders

Every member reaches **every folder** of the shared mailbox, and folders made
later too, with all rights (`lrswipkxtea`, see [sharing.md](sharing.md)): read,
flag, file, delete, create folders and share single folders further. Flags are
the mailbox's, so a message one member read is read for all.

- **IMAP:** the folders appear as `Shared/<address>/…`, for example
  `Shared/support@example.org/INBOX` and `Shared/support@example.org/Sent`.
  `MYRIGHTS` answers `lrswipkxtea`.
- **JMAP and the webmail:** the shared mailbox is an account of its own in the
  member's session (`isPersonal: false`), with the mail capability, like
  someone who shares folders with you. `Principal/get` knows it as a principal
  of `type: "other"` with the id `p<account>`; `Principal/query` leaves it out,
  as nothing is shared with it.
- Push, IDLE and `*/changes` follow it like any shared account.

Where a member also got a single folder of it shared the ordinary way, the
membership decides.

### Storage

Mail in the shared mailbox counts against its own storage limit, whoever filed
it: mail that arrives for it, messages members move or copy into it, and the
copies of what they send as it. Nothing of it counts for the members.

### Sending as it

A member who may send finds its addresses among their own sending addresses:
as JMAP identities (named after the shared mailbox) and as allowed From
addresses over submission. They send from their own account; a JMAP
`EmailSubmission` in the shared account itself answers
`accountNotSupportedByMethod`, as for every shared account.

Everything sent with one of its addresses is also filed, marked as read, into
the **Sent folder of the shared mailbox**, so every member sees what was
answered. The sender's own copy is up to their mail app, as always.

The identities follow the membership and the addresses: taking away the send
right or the membership, removing an alias of the shared mailbox or deleting
it removes them, and a new alias adds one.

### In the trash

A shared mailbox moved to the trash takes no mail and its members no longer
see it or send as it. Restored, it is back as it was.

## Portal API

Admins (session cookie and CSRF header, as the rest of the portal):

| Request | Does |
| --- | --- |
| `POST /api/admin/domains/{domain}/groups` with `{ local, name, whoMaySend, membersMaySendAs, members: [login] }` | creates a group, `201` with it |
| `PATCH /api/admin/domains/{domain}/groups/{local}` with any of `name`, `whoMaySend`, `membersMaySendAs`, `members` | changes it; `members` replaces the list |
| `DELETE /api/admin/domains/{domain}/groups/{local}` | removes it, `204` |
| `GET /api/admin/domains/{domain}` | the domain, with `groups` |
| `GET /api/admin/shared-mailboxes` | `[{ login, name, members: [{ id, login, name, maySend }] }]` |
| `POST /api/admin/shared-mailboxes` with `{ address, name, quotaBytes, members: [{ login, maySend }] }` | creates one, `201` with `{ person, members }` |
| `PUT /api/admin/shared-mailboxes/{login}/members` with `{ members: [{ login, maySend }] }` | replaces the members |
| `POST /api/admin/people/{login}/shared-mailbox` with `{ members: [{ login, maySend }] }` | turns a person or a service into a shared mailbox, `{ person, members }` |
| `DELETE /api/admin/people/{login}/shared-mailbox` | turns a shared mailbox back into a plain service, answers with the person |
| `POST /api/admin/people/{login}/app-passwords` with `{ name }` | an app password for a service or shared mailbox, as for any service |
| `GET /api/admin/people/{login}` | for a shared mailbox, `sharedMailbox: true` and its `members` |

Each change is written to the admin log (`group.create`, `group.update`,
`group.remove`, `sharedMailbox.create`, `sharedMailbox.members`,
`sharedMailbox.convert` with `from: "person" | "service"`, `sharedMailbox.end`).

For everyone, `GET /api/account/addresses` also returns
`groups: [{ address, name, maySendAs }]` and
`sharedMailboxes: [{ id, address, name, maySend }]`.
