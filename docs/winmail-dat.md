# winmail.dat (TNEF)

Outlook and Exchange sometimes send mail in their own format: instead of MIME parts the message
carries one `application/ms-tnef` part, usually called `winmail.dat`, holding the body (as
compressed RTF, often with the original HTML wrapped inside), the attachments and, for meetings,
the invitation. Other mail programs show a useless `winmail.dat` file. UwUMail reads what is
inside.

## The decoder

`crates/uwumail-tnef` decodes TNEF streams (MS-OXTNEF, MS-OXRTFCP, MS-OXRTFEX, MS-OXOCAL,
MS-OXCICAL):

- attachments with their long Unicode names, content ids, media type (the one Outlook recorded,
  else guessed from name and bytes), attached messages decoded in turn;
- the body: `PR_BODY`, `PR_HTML`, and compressed RTF, unpacked. HTML or text wrapped in RTF comes
  out as it was; real RTF becomes text and simple HTML (paragraphs, bold, italic, underline,
  strike-through, links to `http`, `https` and `mailto` only);
- meetings (`IPM.Schedule.Meeting.Request`, `.Canceled`, `.Resp.Pos/Neg/Tent`, `IPM.Appointment`)
  as iCalendar with a METHOD: start and end in the meeting's Windows time zone (with a VTIMEZONE
  built from its rules), all-day events, location, organizer, attendees (from the recipient table,
  else from the mail's To and Cc), the UID Outlook derives from the GlobalObjectId, sequence,
  single instances (RECURRENCE-ID) and recurrence (daily, weekly, monthly, nth weekday, yearly,
  count or end date, deleted instances as EXDATE; changed instances are not carried over).

The input is untrusted: every read is bounds-checked, there is no recursion without a depth limit
(RTF groups use a stack of their own), sizes and counts are limited (`Limits`), and no input
panics; tests feed it random and damaged streams. A damaged stream gives what could be read
before the damage.

The crate depends on no other UwUMail crate (only `encoding_rs`), so the UwUMail client copies it
as it is. Its `builder` feature adds a small TNEF writer for tests.

## Decoded when read, never stored

Stored mail stays byte for byte as it arrived, so DKIM signatures still verify, and nothing
decoded is stored. The TNEF part is decoded whenever something needs what is inside:

- **At delivery** (`uwumail-store` `parse`): the text and the attachment names go into the search
  index, the preview falls back to the TNEF body, and `hasAttachment` counts the attachments inside
  instead of `winmail.dat` itself. Mail stored before this version keeps its old index entries.
- **Calendar** (`uwumail-smtp` `scheduling`): a meeting inside TNEF is taken like an iMIP
  invitation, answer or cancellation, with the same rules for who may say what. A real
  `text/calendar` part wins over TNEF.
- **Spam check**: the attachments inside count like the mail's own (programs, macros, archives
  with programs).
- **AI assistant**: the TNEF body is the mail's text when the MIME has none.
- **JMAP** (`uwumail-jmap` `email`), see below.

Decoding on read instead of storing parts: the decoder is linear in the size of the part and
bounded, it gives the same parts every time (so their ids stay the same), it needs no migration
and no second copy of every attachment, and a better decoder improves old mail too. The cost is
paid only when a mail with TNEF is opened or one of its parts downloaded.

## What JMAP shows

For every `application/ms-tnef` (or `application/vnd.ms-tnef`) part, or part named
`winmail.dat`, whose content is TNEF (at most four per mail):

- `attachments` leaves out the TNEF part and lists instead what it holds, in this order: the
  meeting (when it is one) as `type: "text/calendar"`, `name: "invite.ics"`,
  `disposition: "attachment"`, `charset: "utf-8"`; then each attachment with its name, type,
  `cid`, `location` and `disposition` (`inline` for pictures the HTML shows by `cid:`, else
  `attachment`). Attached messages are `type: "message/rfc822"` with a name ending in `.eml`,
  written out as a MIME message.
- `htmlBody` is the TNEF body's HTML when the MIME has no `text/html` part.
- `textBody` is the TNEF body's text (or its HTML when it only has that) when the MIME body has
  no text at all (Outlook usually adds an empty or missing `text/plain`). When only the text is
  replaced and there is no TNEF HTML, `htmlBody` is the same part.
- `bodyValues` has the values of these parts under their `partId`s, as usual.
- `uwuSafeHtml` and `uwuHasRemoteContent` use the TNEF HTML when it is the `htmlBody`.
- `bodyStructure`, `headers` and the whole message blob stay exactly as the mail came, with the
  TNEF part in them.

Parts made of a TNEF part have the `partId` `<index>.<sub>`, where `<index>` is the TNEF part's
own `partId` and `<sub>` is `text`, `html`, `ics` or the attachment's number from 1 (`2.text`,
`2.html`, `2.ics`, `2.1`), and the `blobId` `p<sha256>_<partId>`. They download through
`/jmap/download` and can be used in `Email/set` (forwarding an attachment) like any other part,
with the same access rules as the message.

IMAP clients get the message as it is.

## Safe Links

Exchange Online rewrites every link of incoming mail to
`https://<region>.safelinks.protection.outlook.com/?url=<the link>&data=…`. The webmail and the
client show the original link (display only; the mail is not changed). Where the server itself
makes lists of links or text out of a mail, it unwraps them too (`uwumail_tnef::safelinks`): the
preview, the search index, and the links and text the AI assistant gets.
