# Shared folders

Anyone on the server can share a folder with other people on the same server:
a team inbox, the invoices, the club's mail. The other person sees it in their
mail app (IMAP), in the webmail and in the UwUMail apps (JMAP), and can use it
as far as the owner allows.

Only people share with people. Services (see the admin panel) neither share nor
receive shares, and an account in the trash takes no part until it is restored.

## Levels and rights

The portal and the webmail offer three levels. Underneath, every share is a set
of [RFC 4314](https://www.rfc-editor.org/rfc/rfc4314) rights, and IMAP clients
may set those letter by letter.

| Level | Rights | May |
| --- | --- | --- |
| Read (`read`) | `lr` | see the folder and read its messages |
| Read and write (`write`) | `lrswite` | also mark messages read, flag them, file new ones in, delete and expunge |
| Everything (`all`) | `lrswipkxtea` | also create folders inside it, rename or delete it, and share it on |

| Letter | Right |
| --- | --- |
| `l` | see the folder in lists |
| `r` | open it and read its messages |
| `s` | keep the seen flag (`\Seen`, `$seen`) |
| `w` | change the other flags and keywords |
| `i` | add messages (APPEND, COPY or MOVE into it, Email/import) |
| `p` | send to it; kept for completeness, nothing here needs it |
| `k` | create folders inside it |
| `x` | rename or delete it |
| `t` | flag messages as deleted, remove them from it |
| `e` | expunge |
| `a` | administer: share it with others |

The owner always has every right on their own folders and is never listed.

A folder created inside a shared folder is shared the same way as its parent,
so whoever made it keeps seeing it. Deleting a folder or an account removes its
shares.

### What stays the owner's

- **Storage.** A shared folder is the owner's. Mail filed into it — by APPEND,
  copying, moving or Email/import — counts against the owner's quota. Copying a
  message *out of* someone else's folder into your own makes a new message in
  your own storage.
- **Flags.** Flags belong to the message, so everyone who sees a shared folder
  sees the same `\Seen`, `\Flagged` and keywords. When someone with the `s`
  right reads a message, it is read for the owner too. There is no per-person
  seen state yet; share with *Read* (no `s`) if the owner's unread markers must
  stay as they are.
- **Subscriptions.** A folder shared with you is always subscribed for you; the
  owner's subscription flag stays the owner's.
- **Roles.** Someone else's Inbox or Sent is not your Inbox or Sent: over IMAP a
  shared folder has no special use, and over JMAP only the owner gives roles.

## In the portal

**My account → Addresses → Shared folders** lists your folders. Share
opens a dialog with the people on the server and the level; each share can be
changed or taken back in place. Below, *Shared with me* lists what others share
with you and at which level.

The portal uses a small JSON API with the usual session cookie and CSRF header:

| Request | Does |
| --- | --- |
| `GET /api/account/sharing` | `{ folders: [{ id, path, role, shares: [{ login, name, level, rights }] }], sharedWithMe: [{ owner, ownerName, id, path, role, level, rights }], people: [{ login, name }] }` |
| `PUT /api/account/sharing/{mailboxId}` with `{ "login": "…", "level": "read" \| "write" \| "all" }` | shares (or changes the level), answers with the view |
| `DELETE /api/account/sharing/{mailboxId}/{login}` | takes it back, answers with the view |

## IMAP

The server announces `ACL` and `RIGHTS=kxte` (RFC 4314) and a second namespace
for other people's folders (RFC 2342):

```
C: a NAMESPACE
S: * NAMESPACE (("" "/")) (("Shared/" "/")) NIL
```

Folders shared with you appear as `Shared/<owner's login>/<the owner's path>`,
for example `Shared/mini@example.org/INBOX` or
`Shared/mini@example.org/Projects/Invoices`. `Shared` and
`Shared/mini@example.org`, and any of the owner's folders above a shared one
that are not shared themselves, are listed as `\Noselect`. If you have a folder
of your own named `Shared`, yours wins where names clash.

Shared folders work with SELECT/EXAMINE, STATUS, FETCH, SEARCH, STORE, APPEND,
COPY, MOVE, EXPUNGE, CREATE, DELETE, RENAME and IDLE, each within the rights:

- SELECT needs `r`; without any of `s`, `t`, `w`, `e` the folder opens
  `READ-ONLY`. `PERMANENTFLAGS` lists only the flags the rights allow.
- FETCH marks messages seen only with `s`.
- STORE leaves flags alone that the rights do not cover and answers
  `NO [NOPERM]` when none are left; replacing all flags (`STORE … FLAGS`)
  needs `s`, `w` and `t`.
- APPEND and COPY/MOVE into a folder need `i`; MOVE out needs `t` and `e`;
  EXPUNGE needs `e`.
- COPY and MOVE between your folders and someone else's make new messages in
  the target account (and its quota); the answer carries `COPYUID` as usual.
- CREATE inside a shared folder needs `k`, DELETE and RENAME need `x`. A shared
  folder stays with its owner: RENAME moves it only within what they shared.
- IDLE in a shared folder hears of the owner's new mail.

Managing shares:

```
C: a SETACL INBOX leni@example.org lrs
C: b SETACL INBOX leni@example.org +wite
C: c GETACL INBOX
S: * ACL "INBOX" "mini@example.org" lrswipkxtea "leni@example.org" "lrswite"
C: d LISTRIGHTS INBOX leni@example.org
S: * LISTRIGHTS "INBOX" "leni@example.org" "" l r s w i p k x t e a
C: e MYRIGHTS "Shared/mini@example.org/INBOX"
C: f DELETEACL INBOX leni@example.org
```

Identifiers are logins of people on this server. `anyone`, `authenticated` and
negative rights (`-login`) are refused with `NO [CANNOT]`, as is the owner.
GETACL, SETACL, DELETEACL and LISTRIGHTS need `a`; MYRIGHTS works on every
folder you can see. The obsolete rights `c` and `d` of RFC 2086 are read as `k`
and `xte`.

## JMAP

Everyone who shares at least one folder with you is an **account of its own**
in your session, with their account id, `isPersonal: false`, and only the mail
capability:

```json
"accounts": {
  "a7": { "name": "leni@example.org", "isPersonal": true, "isReadOnly": false, "accountCapabilities": { … } },
  "a3": {
    "name": "mini@example.org",
    "isPersonal": false,
    "isReadOnly": false,
    "accountCapabilities": {
      "urn:ietf:params:jmap:mail": { "mayCreateTopLevelMailbox": false, … },
      "urn:ietf:params:jmap:principals:owner": { "accountIdForPrincipal": "a3", "principalId": "p3" }
    }
  }
}
```

`isReadOnly` is `true` when every shared folder of that person is read-only for
you. The session state changes whenever a share appears, goes or changes
between read-only and writable.

In a shared account `Mailbox/get|query|queryChanges|changes|set`,
`Email/get|query|queryChanges|changes|set|import|parse|copy`,
`Thread/get|changes` and `SearchSnippet/get` work, limited to the shared
folders:

- Only shared mailboxes are there; a parent that is not shared shows as
  `parentId: null`. `myRights` comes from the rights (`mayReadItems` = `r`,
  `mayAddItems` = `i`, `mayRemoveItems` = `t`+`e`, `maySetSeen` = `s`,
  `maySetKeywords` = `w`, `mayCreateChild` = `k`, `mayRename`/`mayDelete` = `x`,
  `maySubmit` = false, plus `mayAdmin` = `a`).
- Email/query and Email/get see only mail in a shared mailbox you may read;
  `mailboxIds` lists only shared mailboxes. `*/changes` report what went out of
  sight as destroyed, and `*/queryChanges` as removed; they may name ids of
  mail that changed in the owner's other folders, never anything more.
- Email/set checks every keyword and mailbox change against the rights
  (`forbidden` otherwise). Replacing `mailboxIds` never takes a message out of
  the owner's unshared mailboxes. Destroying an email needs `t`+`e` on every
  mailbox it is in.
- `Email/copy` copies between your own account and a shared one, either way
  (and between two shared accounts). From a shared account only mail in a
  folder you may read can be copied, anything else is `notFound`; into one,
  every target folder needs `mayAddItems` (`i`) and the keywords their rights,
  as for Email/import. `onSuccessDestroyOriginal` is an Email/set in the source
  account with its checks, so moving out of a shared folder needs
  `mayRemoveItems` there. Accounts that share nothing with you are
  `fromAccountNotFound`.
- Uploads to `/jmap/upload/{shared account}` are kept as yours and may be used
  by Email/import and Email/set there; `/jmap/download/{shared account}/…`
  serves the messages of the shared mailboxes you may read.
- Push (EventSource) sends changes of a shared account under its account id,
  for `Mailbox`, `Email`, `Thread` and `EmailDelivery`.
- Other methods (identities, submission, vacation, settings, calendars, …)
  answer `accountNotSupportedByMethod` for a shared account: sending always
  happens from your own account. Remote pictures use your own account id.

### shareWith

`Mailbox` has `shareWith` ([RFC 9670](https://www.rfc-editor.org/rfc/rfc9670)):
a map from principal id to a rights object for everyone else the mailbox is
shared with, or `null` when you may not administer it. It is part of the
default properties.

```json
"shareWith": {
  "p7": { "mayReadItems": true, "mayAddItems": false, "mayRemoveItems": false, "maySetSeen": true,
          "maySetKeywords": false, "mayCreateChild": false, "mayRename": false, "mayDelete": false,
          "maySubmit": false, "mayAdmin": false }
}
```

Mailbox/set changes it with the whole map or one principal at a time. A value
is a rights object (missing rights are `false`; any `true` adds `l`), one of
the levels `"read"`, `"write"`, `"all"` as a shortcut, or `null` to stop
sharing:

```json
["Mailbox/set", { "accountId": "a3", "update": {
  "m12": { "shareWith/p7": "write" },
  "m15": { "shareWith": { "p7": { "mayReadItems": true }, "p9": "all" } },
  "m16": { "shareWith/p7": null }
} }, "0"]
```

It needs `mayAdmin` (the owner always has it). Unknown principals and the owner
themselves are `invalidProperties` on `shareWith`.

### Principals

With `urn:ietf:params:jmap:principals` in `using`, `Principal/get`,
`Principal/query` and `Principal/changes` list the people on the server (not
services, not accounts in the trash) in your own account:

```json
{ "id": "p3", "type": "individual", "name": "Mini", "description": null,
  "email": "mini@example.org", "timeZone": null, "capabilities": {},
  "accounts": { "a3": { "name": "mini@example.org", "isPersonal": false, … } } }
```

`accounts` holds the account you can open for that person (your own, or theirs
when they share with you), otherwise `null`. Principal/query filters by
`email`, `name`, `text`, `type` and `accountIds`. Principal/changes answers
`cannotCalculateChanges` whenever someone joined, left or was renamed. Your own
principal id is `currentUserPrincipalId` in your account's principals
capability.

The same principal ids are the keys of `shareWith` for calendars and address
books ([jmap-calendars.md](jmap-calendars.md#shared-calendars),
[jmap-contacts.md](jmap-contacts.md)), and a calendar shared with you names its
owner's principal in `uwuSharedBy.principalId`. Calendars and address books
shared with you are part of your own account, not of the owner's shared
account.

ShareNotification objects are not kept.

## IMAP4rev2

Next to sharing, IMAP speaks IMAP4rev2 ([RFC 9051](https://www.rfc-editor.org/rfc/rfc9051))
alongside IMAP4rev1. Both are announced; a client switches with
`ENABLE IMAP4rev2` and then gets:

- `ESEARCH` answers to every SEARCH (`SEARCH ALL` answers
  `* ESEARCH (TAG "a") ALL 1:9`),
- no `RECENT` and no `[UNSEEN n]` on SELECT, and a `* LIST` answer for the
  mailbox it opened; `[CLOSED]` when a SELECT closes the previous mailbox,
- mailbox names in UTF-8 instead of modified UTF-7.

For every client: `NAMESPACE`, `STATUS … SIZE` and `DELETED`, `LITERAL+` (which
covers IMAP4rev2's `LITERAL-`), `BINARY` (`BINARY[…]`, `BINARY.PEEK[…]`,
`BINARY.SIZE[…]` and `~{n}` literals in APPEND; an unknown transfer encoding
answers `NO [UNKNOWN-CTE]`), `UNAUTHENTICATE`, `SEARCHRES`
(`SEARCH RETURN (SAVE)` and `$`), `LIST-EXTENDED`, `LIST-STATUS`,
`SPECIAL-USE`, `UIDPLUS`, `MOVE`, `UNSELECT`, `ID`. The registered keywords
`$Forwarded`, `$MDNSent`, `$Junk`, `$NotJunk`, `$Phishing` and `$Important`
are written as RFC 9051 spells them.
