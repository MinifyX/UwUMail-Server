# Masked addresses and JMAP MaskedEmail

A masked address is a random address a person makes for one website, such as
`maple.otter482@example.org`. Mail to it arrives like mail to their own
address, but the website never learns the real one. When a masked address
starts getting spam, the person knows who passed it on and turns it off.

UwUMail Server speaks Fastmail's JMAP extension for this
([MaskedEmail](https://www.fastmail.com/for-developers/masked-email/)), so
password managers that make masked addresses for Fastmail can do the same
here. The portal has a page for it, *My account → Masked addresses*.

## Switching it on

Masked addresses are off until an admin opens a domain for them:
*Accounts & domains → a domain → Masked addresses*. People can then make
masked addresses on every open domain. A new one goes on the person's own
domain if that is open, otherwise on the first open domain by name (the portal
lets one choose).

Closing a domain again only stops new ones; the masked addresses already made
keep working. A domain cannot be removed while masked addresses on it still
take mail.

## States

| State | Mail to it |
| --- | --- |
| `pending` | delivered to the Inbox; the first message turns it `enabled` |
| `enabled` | delivered like mail to one's own address: spam filter, sender lists, Sieve rules and forwarding apply |
| `disabled` | accepted without a word and filed into the **Trash**, marked as read, past rules and forwarding |
| `deleted` | refused at RCPT with `550 5.1.1`, and no catch-all takes it |

A `pending` address that got no mail within **24 hours** of being made is
deleted by the hourly housekeeping; this is for addresses a password manager
made for a sign-up that never happened. Addresses made in the portal start
`enabled`, as someone made them by hand to use them.

A masked address can go between `enabled`, `disabled` and `deleted` in any
direction, a deleted one back included, but never back to `pending`.

A masked address is **never handed out again**, not even once it is deleted
or its owner is gone: nobody else can ever get mail meant for it. Its name is
also never the name of a person, an alias, a group or a forwarding address.

`+tags` work: `maple.otter482+news@example.org` reaches the same address.

Mail to a masked address gets no vacation reply, which would give away the
real address.

## Replying as a masked address

The owner may send with a masked address that is not deleted, as From and
envelope sender, over SMTP submission, JMAP and the webmail. Masked addresses
get no JMAP identity of their own, as there may be thousands; a client sends
with any identity and the masked address in the From header and in the
envelope's `mailFrom`, or makes an identity for it with `Identity/set`. Such an
identity goes when the masked address is deleted.

## Limits

- 5000 masked addresses per account that are not deleted.
- `description` and `forDomain` at most 200 characters, `url` at most 2000,
  without control characters (and `url` without spaces).
- `emailPrefix` 1 to 64 characters of `a-z`, `0-9` and `_`; capitals are
  taken as lower case.

## JMAP

### Capability

`https://www.fastmail.com/dev/maskedemail`: an empty object in the session's
`capabilities`, in the account's `accountCapabilities` and in
`primaryAccounts`. A client adds it to `using` to call the methods. It is
there even when no domain is open; making one then answers `forbidden`.

### MaskedEmail

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | immutable, server-set; `x` and a number |
| `email` | `String` | immutable, server-set |
| `state` | `String` | `pending` (default on create), `enabled`, `disabled` or `deleted` |
| `forDomain` | `String` | the site it is for, as an origin like `https://shop.example.com`; `""` when not given |
| `description` | `String` | a short note; `""` when not given |
| `lastMessageAt` | `UTCDate\|null` | server-set: when the latest message arrived |
| `createdAt` | `UTCDate` | immutable, server-set |
| `createdBy` | `String` | immutable, server-set: `JMAP` or `Portal` |
| `url` | `String\|null` | a link back to where it is used, for example a password manager's entry |
| `emailPrefix` | `String\|null` | create-only; put in front of the random part |

The address is `word.word123@domain`, or `prefix.word.word123@domain` with an
`emailPrefix`.

### MaskedEmail/get

Standard `/get` (RFC 8620, section 5.1). `ids: null` returns all of the
account's masked addresses, the deleted ones included, newest first.

### MaskedEmail/set

Standard `/set` (RFC 8620, section 5.3).

- `create` takes `state` (`pending` or `enabled`), `forDomain`, `description`,
  `url` and `emailPrefix`. `created` returns `id`, `email`, `state`,
  `createdAt`, `createdBy`, `lastMessageAt` and `emailPrefix`.
- `update` changes `state`, `forDomain`, `description` and `url`.
- `destroy` deletes the address the way `state: "deleted"` does: it stays,
  refuses mail and shows up with that state.

Errors:

| `type` | When |
| --- | --- |
| `invalidProperties` | a server-set or unknown property is given, `emailPrefix` in an update, a state that is not allowed (a new one `disabled` or `deleted`, any one back to `pending`), a prefix with other characters, or a value that is too long |
| `forbidden` | no domain is open for masked addresses, or the account has 5000 of them |
| `notFound` | the id is not one of the account's masked addresses |

```json
["MaskedEmail/set", {
  "accountId": "a1",
  "create": { "k1": { "forDomain": "https://shop.example.com", "description": "Shop", "emailPrefix": "shop" } }
}, "0"]
```

```json
["MaskedEmail/set", {
  "accountId": "a1",
  "created": { "k1": { "id": "x7", "email": "shop.maple.otter482@example.org", "state": "pending",
                       "createdAt": "2026-09-27T10:00:00Z", "createdBy": "JMAP",
                       "lastMessageAt": null, "emailPrefix": "shop" } },
  …
}, "0"]
```

### MaskedEmail/changes

Standard `/changes` (RFC 8620, section 5.2), an addition to Fastmail's
extension. The state is the account's change number, as for mail. Push
(EventSource and WebSocket) reports `MaskedEmail` changes, including the
ones mail causes: a pending address turning on and `lastMessageAt`.

Masked addresses belong to one's own account; in a shared account the methods
answer `accountNotSupportedByMethod`.

## Portal API

| Request | Does |
| --- | --- |
| `GET /api/account/masked` | `{ addresses: [MaskedAddress], domains: [open domain] }` |
| `POST /api/account/masked` with `{ domain?, description, forDomain, url?, emailPrefix? }` | makes one, `enabled`, `201` with it |
| `PATCH /api/account/masked/{id}` with any of `state` (`enabled`, `disabled`, `deleted`), `description`, `forDomain`, `url` | changes it |
| `DELETE /api/account/masked/{id}` | deletes it, answers with the list |
| `PUT /api/admin/domains/{domain}/masked-addresses` with `{ on }` | admins: opens or closes a domain (in the admin log as `domain.maskedAddresses`) |

The portal's objects use numbers for `id` and seconds since 1970 for the
times; errors carry the codes `maskedDomain`, `maskedLimit`, `maskedPrefix`
and `maskedState`.
