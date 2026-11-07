# Delivering to Microsoft

Outlook.com, Hotmail, Live and Exchange Online (Microsoft 365) turn mail away
more readily than other big providers, and often for reasons that have nothing
to do with the message: a new server address without reputation, a hosting
network Microsoft distrusts, or a domain that does not meet Microsoft's rules
for senders. The server watches for this, tells the admins and the people whose
mail did not arrive, and has a checklist of what Microsoft asks for.

## What the server notices

Every answer a Microsoft mail server gives while mail is being delivered is
looked at (hosts under `protection.outlook.com`, `outlook.com`, `hotmail.com`,
the US government and 21Vianet clouds, or a server that greets with such a
name, also behind a static route). These answers become an *issue*:

| Code | Meaning | Kind |
| --- | --- | --- |
| `S3150`, `S3140` (usually with `550 5.7.1`) | The address, or part of its network, is on Microsoft's block list | blocked |
| `5.7.511`, `5.7.606`, "banned sender" | The sending address is banned | blocked |
| `5.7.708` and other `5.7.6xx`–`5.7.7xx` | Microsoft does not accept traffic from this address, e.g. `5.7.708 Access denied, traffic not accepted from this IP` | blocked |
| `451 4.7.650`, `4.7.500` | Throttled because of the address's reputation; the queue keeps trying | throttled |
| `5.7.515` | The sender domain does not meet the required authentication level (Microsoft's rules for bulk senders from May 2025) | authentication |
| `5.7.509` | DMARC fails and the domain's policy says reject | authentication |

A plain `5.7.1` only counts when it talks about a block list; the same code also
means "this recipient takes no mail from outside", which is nobody's reputation.

Issues are kept per sending address (blocks, throttling) or sender domain
(authentication) and code, with when they were first and last seen and how
often. The address is the one Microsoft names in its answer, else the
connection's own when that is a public one.

A block (the *blocked* kind) is over as soon as mail went through to Microsoft
after the last refusal: the delisting worked. Other issues are over when mail
went through after the last refusal and that refusal is a day old. Behind NAT, where the server does not know the
address Microsoft sees, any mail that went through counts. An admin can also
close an issue by hand, for example after Microsoft delisted the address.

## Held mail

When a Microsoft host refuses mail because it blocks the server's address (the
*blocked* kind above), the mail does not bounce at once. Delisting usually takes
a few hours, so the queue holds it and tries again every 45 minutes, for up to
48 hours from when it was sent (or the queue's own lifetime, if that is
shorter). Only then does it bounce, with the usual Microsoft text.

On the first hold the sender gets a delay notice (a DSN with `Action: delayed`)
saying the mail is late and why, unless the client asked for no delay notices
(`NOTIFY` without `DELAY`). Further tries send nothing more.

Mail is only held when the refusal came from a real Microsoft host, never through
a relay or from a server that only talks like Microsoft: anyone could write
`S3150` to keep mail waiting. Bounces caused by Microsoft blocking the server
never count against the sending account (see [outbound abuse](outbound-abuse.md)).

Mail that bounced anyway (the block lasted longer, or it was refused before the
hold existed) is listed under *Server → Queue → Refused mail*. Once the address
is delisted, **Fixed** on one entry puts it back into the queue, together with
every other refusal with the same cause: for Microsoft the same block list code
(`S3150` for hotmail.com, outlook.com and live.com alike), otherwise the same
reply code to the same domain. Only mail sent by the server's own accounts keeps
a copy for this (up to 10 MB), for the 30 days the list keeps it; bounces, forwards and
entries from before 0.28.1 are only taken off the list, and the toast says to
send them again from *Sent*. Mail of a sender who is suspended, in the trash or
deleted stays on the list (`skipped`). `POST /api/admin/queue/refused/{id}/resend` returns
`{ resent, dropped, skipped }`.

## Who hears about it

- **The overview** shows each open issue in the *Sending* area (`microsoftBlocked`
  and `microsoftAuth` red, `microsoftThrottled` yellow) and a banner at the top
  with the code, the address, how many recipients are held, and a link to
  Microsoft's delisting form, <https://sender.office.com>.
- **Admins get a mail** through the usual [admin alerts](admin-alerts.md): when
  an issue is new, once a day while a block stays red and nobody clicked *Got
  it*, and once more when it is over. Throttling is mailed once.
  The mail for a block names the address and lists the steps to get it lifted:
  1. enter the address at <https://sender.office.com> and ask for delisting,
  2. open the link in Microsoft's confirmation mail,
  3. optionally sign up for SNDS at
     <https://sendersupport.olc.protection.outlook.com/snds/> to watch the
     address's standing.
- **The sender's bounce** says in plain words, in their language, that
  Microsoft is blocking or throttling the server (or refusing the domain), that
  it is not their fault, and that the admins know. Throttled mail only bounces
  once the queue gave up on it.

## The Microsoft page

*Server → Microsoft* lists the issues with what each one means and what to do,
and a checklist of Microsoft's rules for senders to Outlook.com (a must above
5,000 mails a day since May 2025, sensible for everyone):

- SPF, DKIM and DMARC per domain, from the domains' last DNS checks; DMARC at
  least `p=none`, aligned with SPF or DKIM (mail from UwUMail always is: it is
  signed with the domain's key and sent with its address).
- Reverse DNS of every sending address, confirmed forward (FCrDNS). With a
  relay, Microsoft sees the relay's addresses.
- TLS: the server always offers STARTTLS to Microsoft; a relay set to no TLS is
  a warning.
- List-Unsubscribe with one click (RFC 8058) for newsletters sent through the
  server; UwUMail itself sends none.
- While an address is blocked, the page shows the address, how many
  recipients are held, and the three steps above.
- Step by step: registering the addresses with SNDS (Smart Network Data
  Services) and the Junk Mail Reporting Program, and asking for delisting.

## API

| | |
| --- | --- |
| `GET /api/admin/microsoft/issues` | `{ issues, delistUrl, sndsUrl, heldCount }`, open ones first, then those resolved in the last 30 days; `heldCount` is the recipients the queue holds for a block |
| `POST /api/admin/microsoft/issues/{id}/resolve` | Closes an issue (change log `microsoft.resolve`) |
| `GET /api/admin/microsoft/checklist` | The checklist; reverse lookups are kept for an hour |
| `POST /api/admin/microsoft/checklist` | Checks every domain's DNS and the reverse names now |
