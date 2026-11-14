# JMAP extension: your own mail in the backups

People find mail they deleted in the server's backups and take it back
themselves ([backups.md](backups.md#people-search-their-own-mail)). Only the
mail of the logged-in account itself is ever found: an account is matched by
its id and the moment it was made, so mail of an account deleted earlier is not
found by a new one that got the same address.

## Capability

`urn:uwumail:jmap:backups` in the session's `capabilities`, in the
`accountCapabilities` of the person's own account and in `primaryAccounts`, an
empty object. It is there only while backups are set up and the admin lets
people search them (*Backups → People may search the backups*, on by default).
Switched off, the methods answer `forbidden`. A shared account never has it.

## `BackupEmail/query`

Searches the headers of the mail in all snapshots at once. A mail that is in
several snapshots is found once, with all of them.

| Argument | |
|---|---|
| `accountId` | the person's own account |
| `filter` | `text` (in the subject, the senders and the recipients, at most 256 characters), `after` and `before` (UTCDates, of when it was received); each may be left out |
| `limit` | 50 if left out, at most 200 |

Newest first. The answer:

```json
{
  "accountId": "a1",
  "list": [{
    "id": "k17",
    "subject": "Deine eSIM",
    "from": [{ "name": "Telekom", "email": "esim@example.com" }],
    "to": [{ "name": null, "email": "mini@example.org" }],
    "receivedAt": "2026-11-02T09:14:00Z",
    "size": 48211,
    "folder": "Inbox",
    "snapshots": ["2026-11-03T01:30:00Z", "2026-11-04T01:30:00Z"],
    "inMailbox": false
  }],
  "indexing": { "done": 4, "total": 7 }
}
```

`folder` is the folder it was in, `snapshots` when the backups it is in were
made, oldest first, `inMailbox` whether the account has it already (the same
Message-ID, or the same content). While `indexing.done` is below
`indexing.total`, older backups are still being read and more may be found
later.

## `BackupEmail/get`

`ids` (at most 10): the same objects as in the query, with `textBody` (the
mail's text, from its HTML when it has no text part, at most 100,000
characters) and `attachments` (`name`, `type`, `size` each). Ids that are not
the person's, or no longer in any backup, come back in `notFound`. The mail is
read from the backup storage, so this can take a moment.

## `BackupEmail/restore`

`ids` (at most 50). Each mail goes back into the folder `Restored <date of the
newest backup it is in>`, unread, with the date it was received. The answer:

| | |
|---|---|
| `restored` | id → the JMAP id of the folder it went into |
| `alreadyThere` | ids the account has already; nothing is stored twice |
| `notRestored` | id → a SetError: `notFound`, `overQuota` (the mailbox is full; this and every one after it), `invalidEmail` (too deeply nested to be stored safely) |
