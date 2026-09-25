# JMAP extension: address suggestions

While someone types a recipient, the webmail and the apps ask the server which
addresses fit: from the person's address books and from the mail they recently
sent and received. The server has all of it; a client would otherwise have to
download the address books and the mail history first.

## Capability

`urn:uwumail:jmap:suggest` in the session's `capabilities` (an empty object)
and in the account's `accountCapabilities`:

```json
"urn:uwumail:jmap:suggest": { "maxLimit": 50 }
```

A client adds the capability to `using` to call the method below.

## AddressSuggestion/query

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | required |
| `text` | `String` | what was typed so far, at most 256 characters; `""` or `null` for the most used addresses |
| `limit` | `UnsignedInt` | how many, 1 to `maxLimit`; default 10 |

```json
["AddressSuggestion/query", { "accountId": "a3", "text": "ka", "limit": 5 }, "0"]
```

```json
["AddressSuggestion/query", {
  "accountId": "a3",
  "list": [
    { "email": "nyu.katze@cats.example", "name": "Nyu Katze", "source": "contact",
      "sources": ["contact", "sent"], "lastUsedAt": "2026-09-20T08:12:00Z" },
    { "email": "kai@paws.example", "name": "Kai Kralle", "source": "sent",
      "sources": ["sent"], "lastUsedAt": "2026-09-24T17:40:00Z" },
    { "email": "karla@cats.example", "name": "Karla Kater", "source": "received",
      "sources": ["received"], "lastUsedAt": "2026-09-25T06:03:00Z" }
  ]
}, "0"]
```

| Property | Type | |
| --- | --- | --- |
| `email` | `String` | the address as it was written |
| `name` | `String\|null` | the address book's name, otherwise the latest name the address came with |
| `source` | `String` | where it comes from first: `contact`, `sent` or `received` |
| `sources` | `String[]` | every source it appears in |
| `lastUsedAt` | `UTCDate\|null` | the latest message to or from it; `null` for address book entries without mail |

The list is best first. An address fits when the address or a word of the
name starts with the text, less when the domain does, least when the text is
somewhere inside; within the same fit, address book entries come first, then
people the person wrote to, then people who wrote to them — the more often and
the more recently, the higher.

Where the addresses come from:

- **Address books**: every card with an email, in every address book of the
  account.
- **Sent**: the recipients (`To`, `Cc`, `Bcc`) of messages in the Sent
  mailbox.
- **Received**: the senders of all other messages, except those in Junk and
  Trash.

Mail is looked through from the newest, at most 2000 messages that mention
the text. The account's own addresses are never suggested. Errors are
`invalidArguments` for a `text` that is not a string or too long, and for a
`limit` that is not a positive number.

This is only for suggestions: to see or change the address books use
`ContactCard` ([jmap-contacts.md](jmap-contacts.md)).
