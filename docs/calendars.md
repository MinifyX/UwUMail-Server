# Calendars: sharing and invitations

UwUMail Server keeps calendars and address books for everyone, reachable over
CalDAV and CardDAV (Apple Calendar and Contacts, Thunderbird, DAVx5 on
Android) and over JMAP (the webmail and the UwUMail apps). This page explains
the two things that connect people: sharing calendars and address books, and
inviting people to events. The JMAP details are in
[jmap-calendars.md](jmap-calendars.md) and [jmap-contacts.md](jmap-contacts.md).

## Sharing

Anyone can share their calendars and address books with other people on the
same server, under **My account → Calendars & contacts** in the portal, or from
the webmail and apps over JMAP (`shareWith`). Pick the person by their address
and one of three levels:

| Level | They can |
| --- | --- |
| Read | see every entry |
| Read and write | also add, change and delete entries |
| Everything | also rename it, change its colour and description, and share it with others (never with more rights than they have, and never take the owner's away) |

Deleting a calendar stays its owner's. Whoever something is shared with can
leave it again at any time, in the portal, by deleting it in their calendar
app, or by destroying it over JMAP; it stays with its owner.

Where shared things show up:

- **CalDAV/CardDAV:** in the person's own calendar or address book home, as
  `/dav/calendars/<their login>/shared~<id>/` (and the same under
  `addressbooks`). Apple's apps see who shares it (`CS:invite`, `CS:shared`), and
  every app sees from the WebDAV privileges whether it may write, so a
  read-only calendar shows as read-only.
- **JMAP:** next to the person's own calendars and address books, with
  `myRights` saying what they may do and `uwuSharedBy` naming the owner.
- **Push:** every change to a shared calendar, whoever makes it, is pushed to
  everyone who sees it.

A shared calendar always shows the owner's name, colour and events. Alerts
belong to the event and are the same for everybody.

## Invitations (scheduling)

Add people to an event in your calendar app and the server invites them; they
answer in theirs, and the answer lands in your event. This is iTIP
([RFC 5546](https://www.rfc-editor.org/rfc/rfc5546)), done by the server the
way CalDAV scheduling ([RFC 6638](https://www.rfc-editor.org/rfc/rfc6638))
describes it, so Apple Calendar, Thunderbird and DAVx5 with any calendar app
need nothing but the account.

**Between people on this server** nothing goes through mail:

- an invitation appears straight in the attendee's default calendar, waiting
  for their answer (`NEEDS-ACTION`);
- their answer (accept, decline, maybe) goes straight into the organizer's
  event, and deleting an invitation declines it;
- when the organizer changes the time, title or place, everyone's copy is
  updated; a changed time asks for the answer again, anything else keeps it;
- when the organizer deletes the event or takes someone off, their copy is
  marked cancelled.

Someone whose account has calendars switched off gets the invitation as mail
instead.

**With people elsewhere** the same happens by mail (iMIP,
[RFC 6047](https://www.rfc-editor.org/rfc/rfc6047)): invitations, changes and
cancellations go out from the organizer's address through the normal outbound
queue, signed with the domain's DKIM key, with a short text in the language
the person chose for themselves (or the server's) and the event attached for
their calendar app. Answers go back to the organizer the same way.

Mail with an invitation that arrives from elsewhere is delivered as usual, and
the event is put into the recipient's default calendar, waiting for an answer,
so it shows in every calendar app at once. Answers and cancellations that
arrive update the event.

### What the server believes

Invitations by mail are easy to forge, so the server is careful about what it
takes into a calendar:

- An invitation is only taken for the recipient's own addresses, never for an
  organizer of this server (those invite directly, never by mail from
  outside), and not from mail that ended up in Junk. An update of an event
  someone already has is only taken from its organizer.
- An answer only counts when the address it came from is the attendee's and SPF
  or DKIM vouch for that address (DMARC-aligned), and only for that attendee.
- A cancellation only counts when it comes from the event's organizer, vouched
  for the same way.
- The server never answers an invitation on its own. Only a person does, so
  two servers cannot keep sending each other messages.

### CalDAV details

- The principal lists all of the person's addresses in
  `calendar-user-address-set`, so a client knows which attendee it is.
- `schedule-inbox-URL` and `schedule-outbox-URL` point to
  `/dav/calendars/<login>/inbox/` and `.../outbox/`. The inbox stays empty,
  because invitations go straight into the calendars; its
  `schedule-default-calendar-URL` names the default calendar.
- A `POST` of a `VFREEBUSY` request to the outbox answers when people of the
  server are busy (from their own calendars, not those shared with them), as
  Apple Calendar asks when attendees are added. Others are "unknown".
- Events have a `Schedule-Tag` (header and `schedule-tag` property). It stays
  the same when the server only writes an answer into the organizer's copy,
  so a client storing with `If-Schedule-Tag-Match` keeps the answers that came
  in meanwhile.
- `SCHEDULE-AGENT=CLIENT` on an attendee (or on the organizer) leaves that
  person to the client: the server sends them nothing.
- Only events are scheduled, not tasks. Scheduling runs for events in one's own
  calendars; changes someone makes in a calendar shared with them are stored
  but send nothing.

## Signed configuration profiles

The configuration profile for iPhone, iPad and Mac (on the overview of My account, under Apps) sets
up mail, calendars and contacts in one go. When the server has a real
certificate (Let's Encrypt, or files from another CA), the profile is signed
with it, and the device shows it as **Verified** instead of warning about an
unsigned profile. With the self-signed certificate of a test setup it stays
unsigned, as a signature by a certificate nobody knows would prove nothing.

The signature is a CMS `SignedData` with the profile attached and the whole
certificate chain, made by the server itself; no `openssl` is needed.
