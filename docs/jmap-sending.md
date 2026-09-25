# JMAP: sending later, undo send and signatures

What UwUMail Server does with `EmailSubmission` (RFC 8621, section 7) beyond
sending at once, and where the signatures of sending addresses live.

## The undo window

Every `EmailSubmission/set` create that does not name its own time waits a
few seconds before the message goes, so "undo send" is real in every client
and not a trick in one browser tab. How long is the person's choice:

| Where | Key | Values |
| --- | --- | --- |
| JMAP `UserSettings` ([jmap-settings.md](jmap-settings.md)) | `undoSendSeconds` | `0`, `5`, `10`, `20`, `30` (numbers) |
| Portal preferences, My account → Forwarding, away and sending | `mailUndoSend` | `"0"`, `"5"`, `"10"`, `"20"`, `"30"` (strings) |

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

The webmail's own signatures in `UserSettings` (`signature:<id>`, with
`forNew`/`forReplies`) are separate: they belong to the webmail and the apps,
the identity signatures to every JMAP client.
