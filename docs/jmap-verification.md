# JMAP extension: verification codes

The server finds sign-in codes and confirmation links when mail comes in
([verification-codes.md](verification-codes.md)). This extension hands them to
clients: as a property of the Email, as a list of the codes that are still
valid, and as a Web Push the moment one arrives, so a notification can show the
code before the app is even open.

It implements the OMXP draft capability `urn:omxp:params:jmap:verification`
under UwUMail's own name until the draft is settled.

## Capability

`urn:uwumail:jmap:verification` in the session's `capabilities` and in the
`accountCapabilities` of mail accounts, an empty object:

```json
"urn:uwumail:jmap:verification": {}
```

Without the capability in `using`, `Email/get` leaves the property out
entirely, as any server that does not know it would, and
`VerificationCode/query` is an `unknownMethod`.

## Email: `verification`

`Email/get` returns it only when asked for by name in `properties`. It is
`null` for a mail without a code, otherwise:

```json
"verification": {
  "kind": "otp",
  "origin": "login.shop.example",
  "code": "482913",
  "purpose": "login",
  "expiresAt": "2026-11-01T17:05:00Z",
  "source": "header",
  "autofill": true
}
```

| Property | Meaning |
| --- | --- |
| `kind` | `"otp"`: a code to type; `"link"`: a link to open |
| `origin` | the host the code belongs to. For a guess, the registrable From domain |
| `code` | the code (`null` for a link without one): digits without the spaces or hyphens a guess found between them, a code with letters with its hyphens (`K7P-2QX`) |
| `url` | only for a link: an `https` URL on `origin` or a host below it |
| `purpose` | `"login"`, `"signup"`, `"confirm-email"`, `"reset-password"`, `"confirm-action"` or `"other"` |
| `expiresAt` | UTCDate; never more than 24 hours after the mail |
| `source` | `"header"`: the sender said so in a signed `Verification-Info`; `"heuristic"`: a guess from the text |
| `autofill` | whether the client may offer the code for autofill on `origin`: when DMARC passed, a DKIM signature of the From domain passed, or the sender logged in here; not for a guess on SPF alone |

The property is read-only and can't be set or filtered on. A client shows a
`heuristic` code as a suggestion and offers autofill only where `autofill` is
`true` and the site the person is on is `origin` or below it. After
`expiresAt` it shows the code as expired or not at all.

## VerificationCode/query

The "codes" view: the ids of the emails with a code, newest first. Standard
`/query` arguments (RFC 8620 §5.5) with these limits:

| Argument | |
| --- | --- |
| `filter` | `null` or `{ "expired": … }`. `false` (the default): only codes still valid; `true`: only expired ones; `null`: all |
| `sort` | must be absent or empty; codes are always newest first |
| `position`, `anchor`, `anchorOffset`, `limit`, `calculateTotal` | as usual; at most 256 ids per call. An `anchor` not in the filtered list is `anchorNotFound` |

```json
["VerificationCode/query", { "accountId": "a1", "calculateTotal": true }, "0"]
```

```json
["VerificationCode/query", {
  "accountId": "a1", "queryState": "1204", "canCalculateChanges": false,
  "position": 0, "ids": ["e93", "e88"], "total": 2
}, "0"]
```

The ids are Email ids; the client follows up with `Email/get` and
`"properties": ["verification", …]`. There is no `VerificationCode/queryChanges`
(`canCalculateChanges` is `false`): the list is short, and it changes as codes
expire, without any write to the account. A client asks again on an `Email`
state change or when the earliest `expiresAt` passes.

| Error | When |
| --- | --- |
| `unsupportedFilter` | a filter other than `expired`, or a filter that is not an object |
| `invalidArguments` | `expired` is not a boolean or `null` |
| `unsupportedSort` | a non-empty `sort` |

## Web Push

A PushSubscription ([jmap-push.md](jmap-push.md)) whose `types` name
`VerificationCode` and that came with `keys` gets a push of its own for every
code the server keeps:

```json
{
  "@type": "VerificationCode",
  "accountId": "a1",
  "emailId": "e93",
  "verification": { "kind": "otp", "origin": "login.shop.example", "code": "482913", … }
}
```

It is the one push that carries content of a mail, which is why it needs both:

- **Only encrypted.** A subscription without `keys` never gets it; the push
  service sees only ciphertext (RFC 8291).
- **Only when asked for by name.** `types: null` ("everything") does not
  include it. An app that shows codes in notifications adds
  `"VerificationCode"` to its `types` on purpose.

It goes with `Urgency: high`, so a phone in power saving delivers it at once,
and with a `TTL` until the code expires, at least a minute and at most a day:
a code nobody could use any more is not delivered late. It has no `Topic`, so
two codes in a row do not replace each other at the push service. The usual
`StateChange` for the new Email comes as well.

At most 20 code pushes a minute and 60 an hour go to an account, and only a
bounded number are on the way at once; past that a code is not pushed, but
the `StateChange` still is and the code is in `VerificationCode/query`. Codes
are forgotten 30 days after the mail came in: the property is `null` then.
