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

Masked addresses are off until an admin says where people may make them.

### Mail domains and masked-only domains

A domain is one of two kinds, chosen when it is added (*Accounts & domains →
Add domain*, or `uwumail-server domain add <domain> --masked`):

- A **mail domain** is what every domain was before: people, services, shared
  mailboxes, aliases, groups, forwarding addresses, a catch-all, and masked
  addresses if its policy allows them.
- A **masked-only domain** carries masked addresses and nothing else. Nobody
  can put an account (by hand, on the command line, through an LDAP or OpenID
  Connect login, an import or a restore from the trash), an alias (an admin's
  or one people make themselves), a group, a forwarding address or a catch-all
  there, nor let anyone send as any of its addresses; the server refuses with
  the code `maskedOnlyDomain`. Mail to one of its masked addresses is delivered
  as on any domain; mail to any other address there is refused at RCPT with
  `550 5.1.1`. `postmaster@`, `abuse@` and the report addresses
  (`dmarc-reports@`, `tls-reports@`) work as on every domain. DKIM keys, the
  DNS records page, MTA-STS, TLS and DMARC reports are the same as for a mail
  domain, and mail sent as a masked address is signed with its domain's keys.

A mail domain becomes masked-only (*a domain → Masked addresses → Make it
masked-only*, or `domain kind <domain> masked`) only while nothing but masked
addresses is on it: no account, not even one in the trash, no alias, group,
forwarding address, catch-all or permission to send as any of its addresses.
The portal lists what is still in the way. Its own masked address policy goes,
and so does the switch for aliases people make themselves.

A masked-only domain can always become a mail domain again (`domain kind
<domain> mail`). It is then taken out of every policy that named it, on
domains and on people alike; the confirmation says which, and the admin log
keeps it. Its masked addresses keep working.

### Who may make masked addresses where

Each mail domain has a policy for its users: the accounts whose login is on
it, people, services and shared mailboxes alike (*a domain → Masked
addresses*):

| Part | |
| --- | --- |
| Mode | `off`, `own` (on the domain itself), `dedicated` (on masked-only domains) or `both` |
| Masked-only domains | which masked-only domains its users may use, for `dedicated` and `both` |
| Default domain | where a new one goes when no domain is named, for example by a password manager; empty means automatic |

An admin can set each of the three parts differently for one account (*a
person → Masked addresses*); a part left "as the domain" follows the domain.
What holds for an account is its own part where it has one, its domain's
otherwise. The domains it may use are its own login domain for `own` and
`both`, and the masked-only domains for `dedicated` and `both`. The default is
the chosen one while it is allowed; otherwise, or when none is chosen, the
account's own domain if allowed, else the first allowed domain by name. With no
domain allowed, making one fails with `maskedDomain`.

Only new masked addresses follow the policy. The ones already made keep
receiving mail and may be sent as, and their owner can turn them on and off,
delete them and bring them back, whatever the policy says later. A domain
cannot be removed while masked addresses on it still take mail.

Upgrading from 0.15: every domain that was open for masked addresses gets the
mode `own`, all others `off`. People of other domains no longer make new ones
there; the masked addresses they made keep working.

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
`capabilities`, and the account in `primaryAccounts`. A client adds it to
`using` to call the methods. It is there even when the account may make no
masked addresses; making one then answers `forbidden`.

As a UwUMail extension, the account's `accountCapabilities` entry says where
the account may make them:

```json
"https://www.fastmail.com/dev/maskedemail": {
  "domains": ["example.org", "masked.example"],
  "defaultDomain": "masked.example"
}
```

`domains` lists the allowed domains by name (empty when none are),
`defaultDomain` is the one a new masked address goes to without a `domain`
(`null` when none are allowed). Clients that do not know the extension ignore
both. When an admin changes the policy, the session's `state` (and every
response's `sessionState`) changes, so clients fetch the session again.

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
  `url`, `emailPrefix` and, as a UwUMail extension, `domain`: one of the
  capability's `domains` (a name, any case). Without it the new address goes
  to the `defaultDomain`. `created` returns `id`, `email`, `state`,
  `createdAt`, `createdBy`, `lastMessageAt` and `emailPrefix`; the domain is
  the one in `email`.
- `update` changes `state`, `forDomain`, `description` and `url`.
- `destroy` deletes the address the way `state: "deleted"` does: it stays,
  refuses mail and shows up with that state.

Errors:

| `type` | When |
| --- | --- |
| `invalidProperties` | a server-set or unknown property is given, `emailPrefix` or `domain` in an update, a `domain` that is not a string, a state that is not allowed (a new one `disabled` or `deleted`, any one back to `pending`), a prefix with other characters, or a value that is too long |
| `forbidden` | the account may make no masked addresses, the `domain` given is not one it may use, or it has 5000 of them |
| `notFound` | the id is not one of the account's masked addresses |

```json
["MaskedEmail/set", {
  "accountId": "a1",
  "create": { "k1": { "forDomain": "https://shop.example.com", "description": "Shop", "emailPrefix": "shop",
                       "domain": "example.org" } }
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
| `GET /api/account/masked` | `{ addresses: [MaskedAddress], domains: [allowed domain], defaultDomain }` |
| `POST /api/account/masked` with `{ domain?, description, forDomain, url?, emailPrefix? }` | makes one, `enabled`, `201` with it; without `domain` on the default one |
| `PATCH /api/account/masked/{id}` with any of `state` (`enabled`, `disabled`, `deleted`), `description`, `forDomain`, `url` | changes it |
| `DELETE /api/account/masked/{id}` | deletes it, answers with the list |
| `POST /api/admin/domains` with `{ name, kind? }` | admins: adds a domain, `kind` is `mail` (the default) or `masked` |
| `PUT /api/admin/domains/{domain}/kind` with `{ kind }` | admins: turns a domain masked-only or back into a mail domain, answers with the domain (admin log: `domain.kind`, with the policies it was taken out of) |
| `PUT /api/admin/domains/{domain}/masked-policy` with `{ mode, maskedDomains, defaultDomain }` | admins: a mail domain's policy, answers with the domain (admin log: `domain.maskedPolicy`) |
| `PUT /api/admin/people/{login}/masked-policy` with `{ mode, maskedDomains, defaultDomain }`, each `null` for "as the domain" | admins: one account's own parts, answers with `{ custom, domain, effective, choices }` (admin log: `account.maskedPolicy`) |

The domain list and a domain's detail carry its `kind`; the detail of a mail
domain carries `maskedPolicy` and `kindBlockers` (`{ accounts, aliases,
groups, forwards, catchAll, sendAs }`, what keeps it from turning
masked-only), the detail of a masked-only domain `maskedUsedBy` (`{ domains,
accounts }`, the policies that name it); both list the masked-only domains as
`maskedDomainChoices`. A person's detail carries `maskedPolicy` like the answer
above: `custom` (their own parts), `domain` (their domain's policy),
`effective` (`{ mode, maskedDomains, domains, defaultDomain }`) and `choices`.

The portal's objects use numbers for `id` and seconds since 1970 for the
times; errors carry the codes `maskedDomain`, `maskedLimit`, `maskedPrefix`,
`maskedState`, `maskedOnlyDomain` (something other than a masked address on a
masked-only domain), `notMaskedDomain` (a policy names a domain that is not
masked-only), `maskedDefault` (a default domain the policy does not allow) and
`kindChangeBlocked` (a `409` whose `blockers` says what is still on the
domain).
