# Outbound abuse

A taken-over account, or someone who sends to a bought address list, can get the
server's address onto block lists within hours. Then mail from everyone on the
server is turned away. So the server watches what its own people send to other
servers and stops an account for a while when it looks like abuse.

Mail that stays on the server is not affected by anything here. It is judged on
arrival like any other mail ([spam-filter.md](spam-filter.md)).

## The outbound spam scan

Mail submitted over SMTP (ports 587 and 465) or JMAP is scanned before it is
queued, if at least one recipient is on another server. The scan uses what the
message itself shows:

- the word lists and the Bayes filter,
- the link and URI lists (URLhaus, domain block lists for links, shorteners),
- the virus scanner (ClamAV), when one is set up.

Rules about the sending server do not apply: there is no other server, so SPF,
reverse DNS, greylisting, IP block lists and the HELO greeting are skipped.

A message with a score at or above the reject threshold (`spam.reject_score`, or
10 points when none is set) is refused. SMTP answers
`550 5.7.1 This message looks like spam and was not sent`, and JMAP answers
`forbiddenToSend`. The refusal is logged with the rules that matched, and it
counts as a strike.

Accounts limited to internal recipients ("only send inside") are not scanned,
because they cannot reach other servers anyway.

Mail for people on this server is scanned too when it would go on to another
server through their forwarding (or through a forwarding address). If it looks
like spam, or the sender is suspended, it stays here: the person still gets it,
only the forward elsewhere is left out. That counts as no strike.

## Strikes and suspensions

These count as a strike against the sending account:

| Strike | When |
| --- | --- |
| `spam` | The outbound spam scan refused a message |
| `rateLimit` | The account hit its sending limit (`smtp.max_recipients_per_hour` / `_per_day`). Counted at most once per strike window, so the limit alone never suspends anyone; it only adds to other strikes |
| `complaint` | An ARF complaint about the account's mail came in (see below) |
| `bounce` | Another server said a recipient does not exist (5.1.0, 5.1.1, 5.1.10). Counted separately, see below |

When an account collects `smtp.suspend_after_strikes` strikes (default 3) within
`smtp.strike_window_minutes` (default 60), or `smtp.suspend_after_hard_bounces`
hard bounces (default 25) in the same window, it may not send to other servers
for `smtp.suspend_minutes` (default 60). A value of 0 for either limit turns
that limit off. All four are live settings in *Server → Settings → Receiving*.

Hard bounces have their own limit because many addresses that do not exist is
what sending to a bought list looks like, but a newsletter to an old list also
bounces a lot. Only mail the person sent counts, with their own address as the
return path: forwarded mail and vacation replies do not, since whoever sends
mail to the account would decide about those. A message counts once per
recipient domain, however many of its recipients there failed. A full mailbox,
a message that is too big, a policy refusal and Microsoft blocking the server's
address ([microsoft.md](microsoft.md)) never count: they say nothing about the
list the mail went to.

While an account is suspended:

- mail to other servers is refused. SMTP answers `550 5.7.1 Sending to other
  servers is suspended for this account for N more minutes; ask your admin if
  this is a mistake`. JMAP answers `forbiddenToSend` with the same text.
- mail inside the server still works.
- strikes during the suspension are still recorded, but they do not make it
  longer.

The suspension ends by itself. Suspensions are stored, so they survive a
restart.

## For admins

- **Alert.** Every suspension raises a `sendingSuspended` alert (one per
  account) through the usual [admin alerts](admin-alerts.md). The alert names
  the account and goes away when the suspension ends or is lifted.
- **Server → Logs → Sending** lists the current suspensions with the reason and
  the time left. *Lift now* ends a suspension early and forgets the strikes that
  led to it, for example after the account's password was changed. Lifting is
  written to the change log as `sending.lift`.
- The same page lists the complaints that came in (below), newest first.

## Complaints (ARF, Microsoft JMRP)

Feedback loops such as Microsoft's Junk Mail Reporting Program (JMRP) send a
report whenever one of their users marks a message from the server as junk. The
report is an ARF message (RFC 5965): `multipart/report;
report-type=feedback-report`, with a `message/feedback-report` part and the
original message attached as `message/rfc822`, or only its headers as
`text/rfc822-headers`. Microsoft sends from `staff@hotmail.com`, uses
`Feedback-Type: abuse`, and removes the recipient address from the original.

Set the feedback loop to send to `abuse@` one of the server's domains. That
address has to exist (as an account, alias or group) like any other. Mail to
`abuse@` is always delivered normally. In addition, an ARF report in it is read
and stored. Mail that is not an ARF report is only delivered.

The report is tied to the account that sent the original message:

1. by the original's `Message-ID`. The server notes the Message-ID of every
   message it sends to other servers, for 30 days. This is the reliable way:
   the ID is in the report even when the recipient is removed.
2. otherwise by the original's `From`, when its domain is one of the server's
   own and the original carries a DKIM signature from that domain. This is only
   shown, never counted: anyone who got a signed message from the account (an
   automatic vacation reply will do) could report it again and again.

A Message-ID is chosen by the mail client. When two accounts sent the same one,
it belongs to neither, so one account cannot turn complaints about its own mail
onto another.

The recipient address is never used: it is removed or made up in most reports.

A report counts as a strike only when all of these are true:

- the report itself passed DMARC (it really comes from the feedback loop's
  domain),
- it is about spam (`abuse`, `fraud`, `virus`; not `not-spam`,
  `auth-failure` or anything else),
- it is tied to an account by its Message-ID,
- it comes from where the mail went: the report's `From` domain belongs to the
  same registrable domain as one of the original's recipient domains, or it is
  Microsoft's JMRP (`staff@hotmail.com`, for all of Outlook.com, Hotmail, Live
  and MSN),
- no earlier report about the same Message-ID counted, and
- no other report from the same domain counted against the account within the
  strike window (JMRP excepted, since it speaks for many people).

Reports are read with the same limits as DMARC and TLS reports: at most 4 MB,
at most four at once, and on a blocking thread so that a broken report cannot
hold up delivery. Each text field is cut to 300 characters. Reports are kept
for 180 days, at most 5,000.

## Login block exemptions by host name

`login_block_exempt` (*Server → Logs → Blocks*, at most 200 entries) also
accepts host names, for an office or home line with a changing address and a
dynamic DNS name. The server resolves each name every 5 minutes (A and AAAA)
and also right after the list changes. At most 16 addresses per name are used.
When a lookup fails, the last addresses stay in place.

Exactly the resolved addresses are exempt, not their networks. Only public
addresses are used: a name that points at loopback, a private network
(10/8, 172.16/12, 192.168/16, 100.64/10, fc00::/7) or a link-local address
exempts nothing. Put networks of your own into the list as networks. Whoever
controls the name (the dynamic DNS account) controls the exemption. A name only
affects the login block. It changes nothing about sending, spam or anything
else. The Blocks page shows each name with the addresses it resolved to.

Names are checked strictly: letters, digits and hyphens, labels of 1 to 63
characters, at least two labels, at most 253 characters, and a last label that
is not a number.

## API

| | |
| --- | --- |
| `GET /api/admin/sending/suspensions` | `{ suspensions: [{ accountId, login, since, until, reason }], now }` |
| `POST /api/admin/sending/suspensions/{accountId}/lift` | Ends a suspension early (change log `sending.lift`); 404 when there is none |
| `GET /api/admin/abuse-reports` | `{ reports }`, the 500 newest complaints |
| `GET /api/admin/blocks` | Now also returns `exemptHosts: [{ host, addresses }]`; `exempt` lists networks and host names as entered |
