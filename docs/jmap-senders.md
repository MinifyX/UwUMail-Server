# JMAP extension: sender lists

UwUMail Server offers one's own allowed and blocked senders over JMAP, so the
UwUMail apps can block a sender on the server instead of on one device. It is
the same list as under *Mein Konto → Spamfilter* in the portal; see
[spam-filter.md](spam-filter.md#allowed-and-blocked-senders) for what the
entries do.

## Capability

`urn:uwumail:jmap:senders` in the session's `capabilities` (an empty object) and
in the account's `accountCapabilities`:

```json
"urn:uwumail:jmap:senders": { "maxEntries": 1000 }
```

A client adds the capability to `using` to call the methods below.

## SenderList

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | immutable, server-set |
| `list` | `String` | `allow` or `block` |
| `kind` | `String` | `ip`, `host`, `address`, `domain` or `pattern`; guessed from `value` when left out on create |
| `value` | `String` | stored normalized: lower case, ASCII domains, masked networks, no leading `@` |
| `note` | `String` | free text, at most 200 characters |
| `createdAt` | `Int` | seconds since 1970, server-set |

### SenderList/get

Standard `/get` (RFC 8620, section 5.1). `ids: null` returns the whole list.
The `state` changes whenever an entry is added or removed.

### SenderList/set

Standard `/set` (RFC 8620, section 5.3) with `create` and `destroy`. Entries
cannot be updated; destroy one and create another. `created` returns `id`,
`kind`, `value` and `createdAt`, so the client learns the stored form.

Errors in `notCreated`:

| `type` | When |
| --- | --- |
| `invalidProperties` | `list` or `value` missing, or `list`/`kind` has an unknown value |
| `senderInvalid` | the value is not what `kind` says, a network is wider than /8 or /16, a domain has no dot, or a pattern has fewer than three characters besides `*` |
| `senderListed` | the value is already on one of the two lists |
| `senderListFull` | the list holds `maxEntries` entries |

```json
["SenderList/set", {
  "accountId": "a1",
  "create": { "k1": { "list": "block", "value": "news@shop.example" } }
}, "0"]
```
