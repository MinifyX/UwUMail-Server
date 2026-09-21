# JMAP extension: user settings

UwUMail Server keeps one settings document per account, so the webmail and the
UwUMail apps on every device show the same choices: theme, tone, trusted
senders, signatures and so on. Theme, tone, language and four of the
webmail's mail choices are the same values the portal stores as preferences;
a change in the portal shows up here, and a change here shows up in the
portal.

## Capability

`urn:uwumail:jmap:settings` in the session's `capabilities` (an empty object)
and in the account's `accountCapabilities`:

```json
"urn:uwumail:jmap:settings": { "maxKeys": 5000, "maxSize": 1048576, "maxValueSize": 262144 }
```

| Limit | |
| --- | --- |
| `maxKeys` | how many keys `values` may hold, and how many one update may name |
| `maxSize` | how many bytes `values` may take, as a JSON object |
| `maxValueSize` | how many bytes one value may take, as JSON |

A client adds the capability to `using` to call the methods below. The
webmail, which signs in with the portal's session, gets the same session
document.

## UserSettings

A singleton like RFC 8621's `VacationResponse`.

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | always `singleton` |
| `values` | `String[*]` | a flat map of keys to JSON values, see [Keys](#keys) |

### UserSettings/get

Standard `/get` (RFC 8620, section 5.1). `ids` is `null` or `["singleton"]`;
any other id is in `notFound`. `properties` may pick `id` and `values`. The
`state` changes with every write, from here or from the portal, and not with
mail.

```json
["UserSettings/get", { "accountId": "a1", "ids": null }, "0"]
```

```json
["UserSettings/get", {
  "accountId": "a1",
  "state": "42",
  "list": [{ "id": "singleton", "values": {
    "theme": "dark",
    "conversations": true,
    "trustedSenders:@shop.example": true
  } }],
  "notFound": []
}, "0"]
```

### UserSettings/set

Standard `/set` (RFC 8620, section 5.3) with `ifInState`. Only `update` of
`singleton` does anything; `create` and `destroy` end up in `notCreated` and
`notDestroyed` with the type `singleton`, and any other id in `update` is
`notFound`.

The update is a PatchObject. Each key is `values/<key>` with the setting's key
as a JSON pointer token (`~1` for `/`, `~0` for `~`) and the new value, or
`null` to remove the key. A value is always set whole: a path that points
inside one (`values/signature:work/html`) is `invalidPatch`. Instead of single
keys the patch may give `values` as a whole object, which replaces all
settings, the portal's included; the two forms cannot be mixed
(`invalidPatch`). `updated` is `{ "singleton": null }`.

```json
["UserSettings/set", {
  "accountId": "a1",
  "ifInState": "42",
  "update": { "singleton": {
    "values/theme": "light",
    "values/trustedSenders:news@shop.example": true,
    "values/linkDomains:example.net": null
  } }
}, "0"]
```

The update is checked as a whole before anything is written: one bad key and
none of it is kept. Writes that reach the server later win, key by key.

Errors in `notUpdated`:

| `type` | When |
| --- | --- |
| `invalidProperties` | a key is not on the list below, or its value is not allowed; `properties` lists the offending keys as plain keys (`colour`, not `values/colour`), and any property other than `values` or `id` by its name |
| `invalidPatch` | a path points inside a value, has a bad `~` escape, or `values` and `values/…` are mixed |
| `tooLarge` | one value is larger than `maxValueSize` |
| `overQuota` | afterwards there would be more than `maxKeys` keys or more than `maxSize` bytes, or the update names more than `maxKeys` keys (removals and `null`s count too) |

A stale `ifInState` fails the whole call with `stateMismatch`, also when
another write slipped in between the check and the write.

### Push

Every write moves the state and sends a `StateChange` for the type
`UserSettings` over the EventSource (`/jmap/eventsource`), with the new
`UserSettings` state:

```
event: state
data: {"@type":"StateChange","changed":{"a1":{"UserSettings":"43"}}}
```

A change of a synced preference in the portal is pushed the same way. A client
that sees a state it does not have calls `UserSettings/get`.

## Keys

Only these keys are accepted. Values are untrusted: whatever one device writes
is handed to the others.

| Key | Value |
| --- | --- |
| `theme` | `"system"`, `"light"` or `"dark"` |
| `tone` | `"playful"` or `"neutral"` |
| `language` | `"system"`, `"de"` or `"en"` |
| `conversations` | `true` or `false` |
| `remoteImages` | `"ask"` or `"always"` |
| `mailAppearance` | `"auto"`, `"light"` or `"dark"` |
| `senderPictures` | `true` or `false` |
| `undoSendSeconds` | `0`, `5`, `10`, `20` or `30` |
| `linkConfirm` | `true` or `false`: ask before opening links from mails |
| `trustedSenders:<entry>` | `true`; `<entry>` is a lower-case address `a@b.c` or `@domain`, at most 254 characters |
| `senderAppearance:<address>` | `"light"` or `"dark"`; `<address>` is a lower-case address, at most 254 characters |
| `linkDomains:<domain>` | `true`; `<domain>` is a lower-case ASCII host name (IDNs in punycode), at most 253 characters |
| `signature:<id>` | `{ "email", "name", "html", "forNew", "forReplies" }`, see below; `<id>` is 1 to 64 of `A-Z a-z 0-9 _ -` |

A signature has exactly these properties: `email` a mail address or `""` for
none, `name` at most 100 characters, `html` a string, `forNew` and
`forReplies` booleans. The server stores `html` as it is; every client cleans
it before use, like mail HTML, and images are only `data:` URIs. Its size is
bounded by `maxValueSize` for the whole value.

Lists are one key per entry, so two devices adding to a list at the same time
never overwrite each other; removing an entry is setting its key to `null`.

### Shared with the portal

These keys are not stored twice. They are the account's portal preferences,
where the two booleans are spelled `"on"`/`"off"`:

| Key | Portal preference |
| --- | --- |
| `theme` | `theme` |
| `tone` | `tone` |
| `language` | `language` |
| `conversations` | `mailConversations` |
| `remoteImages` | `mailRemoteImages` |
| `mailAppearance` | `mailAppearance` |
| `senderPictures` | `mailSenderPictures` |

Portal preferences that only concern one device (`motion`, `mailDensity`,
`mailSwipeLeft`, `mailSwipeRight`, `mode`) are not part of the settings and
not touched by a replacing update.
