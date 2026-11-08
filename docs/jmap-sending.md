# JMAP: sending later, undo send, sending only internally and signatures

What UwUMail Server does with `EmailSubmission` (RFC 8621, section 7) beyond
sending at once, and where the signatures of sending addresses live.

## The undo window

Every `EmailSubmission/set` create that does not name its own time waits a
few seconds before the message goes, so "undo send" is real in every client
and not a trick in one browser tab. How long is the person's choice:

| Where | Key | Values |
| --- | --- | --- |
| JMAP `UserSettings` ([jmap-settings.md](jmap-settings.md)) | `undoSendSeconds` | `0`, `5`, `10`, `20`, `30` (numbers) |
| Portal preferences, My account → Forwarding & sending | `mailUndoSend` | `"0"`, `"5"`, `"10"`, `"20"`, `"30"` (strings) |

Both are the same value; a change on one side shows up on the other and is
pushed as a `UserSettings` change. Without a choice the window is **10
seconds**. `0` sends at once, as before.

While it waits, the submission is `undoStatus: "pending"` and its `sendAt` is
when it will go:

```json
["EmailSubmission/set", {
  "accountId": "a3",
  "create": { "s1": { "identityId": "i4", "emailId": "e42" } },
  "onSuccessUpdateEmail": { "#s1": { "mailboxIds/m2": null, "mailboxIds/m5": true, "keywords/$draft": null } }
}, "0"]
```

```json
["EmailSubmission/set", {
  "accountId": "a3",
  "created": { "s1": { "id": "s9", "threadId": "t7", "undoStatus": "pending", "sendAt": "2026-09-25T10:00:10Z" } },
  …
}, "0"]
```

`onSuccessUpdateEmail` runs at once (the message moves to Sent right away); a
client that cancels moves it back itself.

## Cancelling

```json
["EmailSubmission/set", { "accountId": "a3", "update": { "s9": { "undoStatus": "canceled" } } }, "0"]
```

The only change a submission takes, and only while it is `pending`. Once the
message is on its way the update fails with `cannotUnsend`; any other property
is `invalidProperties`. Destroying a pending submission also stops it.

## Sending later

A submission names its own time in one of three ways; each skips the undo
window:

- `sendAt` (a `UTCDate`) in the create — an extension of this server; RFC 8621
  has `sendAt` server-set.
- `envelope.mailFrom.parameters.HOLDFOR`: seconds to wait (RFC 4865
  FUTURERELEASE).
- `envelope.mailFrom.parameters.HOLDUNTIL`: a date to wait for (RFC 4865).

A time in the past means now. At most 30 days ahead; further is
`invalidProperties`. The session says so:

```json
"urn:ietf:params:jmap:submission": {
  "maxDelayedSend": 2592000,
  "submissionExtensions": { "FUTURERELEASE": ["2592000", "2026-10-25T10:00:00Z"] }
}
```

(`FUTURERELEASE`'s arguments are its EHLO arguments: the longest hold in
seconds and the latest date.)

## What happens while a message waits

A held submission is checked when it is made — sender, recipients, limits,
whether the account may send at all — so a message that could never go is
refused at once (`forbiddenFrom`, `forbiddenMailFrom`, `forbiddenToSend`, …)
and not silently later. It is checked again when it goes.

The message itself is kept as it was when it was submitted: editing or
deleting the draft afterwards changes nothing about what is sent. It is stored
in the database with its release time, not in memory, so a restart only
delays it; a message that was being handed over when the server stopped is
sent after the restart (possibly twice, never not at all).

When its time comes, the submission turns `final` and the message goes
through the same path as one sent at once (`Smtp::submit`: DKIM, local
delivery, the queue). If it cannot go then — the account was switched off
meanwhile, for example — `deliveryStatus` has an entry per recipient with
`delivered: "no"` and the reason in `smtpReply`.

Waiting mail belongs to the login that held it back, so that someone who had
an account in their hands for a moment cannot leave a month of mail behind.
It is not sent, but turns `canceled` (with the reason kept as its release
error), when:

- the account is disabled or moved to the trash — even if it is switched back
  on before the time comes;
- the app password or the OAuth app it was held with is revoked;
- the password changes, for what was held with the password or from the
  webmail. Changing one's own password in the portal keeps what that very
  browser session scheduled; an admin setting the password, or a reset link,
  stops all of it.

Logging out of the webmail stops nothing: scheduled mail survives the session.

## Sending only within this server

An admin can mark an account as one that may only send to addresses on this
server: Admin → People → the person → Protocols → "Only send within this
server" (`PATCH /api/admin/people/{login}` with `{"internalOnly": true}`,
logged as `account.update`), or on the command line with
`uwumail-server account protocols someone@example.com --internal-only on`.
It is off for everyone. On the inside counts
every domain this server holds, mail and masked-only domains alike; an
unknown address on one of them is refused as unknown, as for anyone else.

The switch is read from the account as it is now, at every door:

| Where | Recipient elsewhere | Nobody here at all |
| --- | --- | --- |
| SMTP submission (587/465) | `RCPT` answered `550 5.7.1 This account may only send to addresses on this server`; the recipients here still get the message | every `RCPT` refused, so the client has nobody to send to |
| JMAP `EmailSubmission/set`, sent at once | the message goes to the recipients here; the others come back to the sender in a bounce with the same `550 5.7.1` reason | `forbiddenToSend`, nothing is sent and nothing bounces (like a message nobody accepts) |
| JMAP, held back (undo window, send later) | checked again when it goes: the same split | refused with `forbiddenToSend` when it is held; if the switch went on meanwhile, it does not go and `deliveryStatus` names the reason |
| Sending as another account's address (a shared mailbox, a service account, a send-as domain) | the switch counts when it is on for the person sending or for the account the address belongs to: `MAIL FROM`, `From` and `Sender` are all looked at, so a shared mailbox that is switched on stays inside for every member who writes as it | as for the account itself |

What else stays inside while it is on:

- **Forwarding** only goes to people on this server: an address elsewhere,
  and also an address on one of this server's domains that belongs to nobody
  here, cannot be set up (`internalOnly` error), because forwarding would hand
  it to the domain's public mail server. Targets set up before are skipped
  when mail arrives and work again once the switch is off. Sieve `redirect`
  only goes where forwarding may, so it follows. Forwarding to people on this
  server is not affected.
- **Forwarding addresses** an admin set up (an address without a mailbox that
  passes mail on) still reach their targets here when an internal-only
  account writes to them; their targets elsewhere are left out, and the sender
  gets a bounce for the forwarding address with the same `550 5.7.1` reason.
- **People here who forward elsewhere** get the message, also through a group,
  and it stays with them even if they keep no copies otherwise: their forwarding
  to other servers stops for this message. Their forwarding to people on this
  server still goes. The sender is not told, because where someone forwards
  their mail is theirs to know.
- **Away replies** only answer senders on this server.
- **Sending through a fetch account's provider** is switched off with the
  switch and cannot be switched back on (`internalOnly` error).
- **Messages already in the outgoing queue** when the switch goes on do not
  go out either: before each delivery attempt the queue looks at the switch
  of the account that queued the message, and its recipients fail with the
  same `550 5.7.1` reason and come back to the sender in a bounce. Mail the
  server sends itself (bounces, reports) belongs to no account and is not
  affected.

The JMAP session tells clients, in the account's
`urn:ietf:params:jmap:submission` capability, so they can warn before the
person writes to someone elsewhere:

```json
"urn:ietf:params:jmap:submission": {
  "maxDelayedSend": 2592000,
  "uwuInternalOnly": true,
  "uwuInternalDomains": ["example.org", "masked.example"],
  "submissionExtensions": { "FUTURERELEASE": ["2592000", "2026-12-03T10:00:00Z"] }
}
```

`uwuInternalOnly` is always there; `uwuInternalDomains` (lowercase, sorted)
only while it is `true`. The session `state` changes when the switch does,
and while it is on also when a domain is added or removed, so a client reads
the session again and gets the new list.

## Signatures

Every sending address is an `Identity` with `textSignature` and
`htmlSignature` (RFC 8621, section 6). They are kept on the server, set with
`Identity/set`, and the same ones the portal edits under My account →
Forwarding, away and sending. Each may take up to 256 KiB; the HTML is stored
as it is and every client cleans it before showing it, like mail HTML.

```json
["Identity/set", { "accountId": "a3", "update": { "i4": {
  "textSignature": "Nyu\nexample.com",
  "htmlSignature": "<p><b>Nyu</b><br>example.com</p>"
} } }, "0"]
```

Since 0.22 these are the **effective** signatures: an address's own, else its
domain's, else the one for every domain, else the domain's company template,
with placeholders like `{name}` filled. Writing one with `Identity/set` makes it
the address's own; writing back what `Identity/get` returned changes nothing.
Signatures per domain and company signatures are described in
[signatures.md](signatures.md) and managed with
[`urn:uwumail:jmap:signatures`](jmap-signatures.md).

The webmail's own signatures in `UserSettings` (`signature:<id>`, with
`forNew`/`forReplies`) are separate: they belong to the webmail and the apps,
the identity signatures to every JMAP client.
