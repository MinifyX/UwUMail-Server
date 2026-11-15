# JMAP extension: shipments

UwUMail Server follows parcels from the mail people get anyway: shipping
confirmations, "out for delivery" notes and pickup notices from shops and
carriers. Each parcel becomes a `Shipment` with its carrier, tracking number,
status and history. The webmail shows them, and apps such as Home Assistant can
read them and write the carrier's live status back.

Everything happens on the server and without AI: the same rules as the
[labels](labels.md#shipments) read the mail when it arrives. **Links in a mail
are never opened**, tracking pages are never fetched, and the server sends no
Web Push of its own for parcels. Shipments are kept per account.

## Capability

`urn:uwumail:jmap:shipments` in the session's `capabilities` (an empty object)
and in the `accountCapabilities` of the person's own account only:

```json
"urn:uwumail:jmap:shipments": { "maxEvents": 200, "maxRescanDays": 90 }
```

| Property | |
| --- | --- |
| `maxEvents` | events one shipment keeps, and that one `addEvents` may bring |
| `maxRescanDays` | the furthest `Shipment/rescan` looks back, in days |

Accounts others share with the person (shared mailboxes, groups) have no
shipments; parcels are found in the mail that arrives in the person's own
mailbox. An app password with the usage `mail` is enough.

## Shipment

```json
{
  "id": "s7",
  "carrier": "dhl",
  "number": "00340434000000000000",
  "trackingUrl": "https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434000000000000",
  "title": null,
  "merchant": "Beispielshop",
  "orderRef": "12345",
  "status": "outForDelivery",
  "statusAt": "2026-11-15T08:12:00Z",
  "expected": { "from": "2026-11-15T10:15:00+01:00", "to": "2026-11-15T12:15:00+01:00", "allDay": false },
  "deliveredAt": null,
  "pickup": null,
  "hidden": false,
  "emailIds": ["e42", "e57"],
  "events": [
    { "at": "2026-11-14T18:03:00Z", "status": "shipped", "text": "Sendung wurde an DHL übergeben", "location": null, "source": "mail", "emailId": "e42" },
    { "at": "2026-11-15T08:12:00Z", "status": "outForDelivery", "text": "Zustellung heute", "location": "Musterstadt", "source": "client", "clientName": "Home Assistant" }
  ],
  "created": "2026-11-14T18:03:10Z",
  "updated": "2026-11-15T08:12:04Z"
}
```

| Property | |
| --- | --- |
| `carrier` | `dhl`, `deutschePost`, `dpd`, `hermes`, `gls`, `ups`, `amazonLogistics`, `fedex` or `other` |
| `number` | the tracking number, upper case without spaces; `null` while only the order is known |
| `trackingUrl` | the carrier's link from the mail, else its tracking page for the number; `null` without either. Never opened by the server |
| `title` | the person's own name for the parcel, at most 200 characters |
| `merchant`, `orderRef` | the shop and its order number, as the mail names them |
| `status` | `ordered`, `shipped`, `inTransit`, `outForDelivery`, `delivered`, `readyForPickup`, `deliveryFailed`, `returned` or `cancelled` |
| `statusAt` | when the status was reached |
| `expected` | when the parcel comes, or `null`. With a time window `allDay` is `false` and `to` is `null` when the mail names only a start. A day without times has `allDay: true`, `from` that day's midnight and `to` the next midnight. Times are local to Europe/Berlin with their offset |
| `deliveredAt` | when it was delivered, or `null` |
| `pickup` | `{ "kind": "packstation" \| "postOffice" \| "parcelShop" \| "accessPoint", "name", "address", "until" }` (`until` a date, `YYYY-MM-DD`), or `null` |
| `hidden` | the person put it out of sight |
| `emailIds` | the mails that told of it, oldest first |
| `events` | the history, oldest first; `source` is `mail` (with `emailId`) or `client` (with `clientName`) |

### How mails become shipments

- A mail counts when the rules are sure enough (confidence 0.8, see
  [labels.md](labels.md#shipments)); junk and newsletters about free shipping
  do not.
- The same number of the same carrier (or of any carrier when the mail names
  none) is the same shipment. An order confirmation without a number becomes
  the shipment once the shipping mail with the same order number or from the
  same shop arrives.
- **The status only moves forward**: `ordered` → `shipped` → `inTransit` →
  `outForDelivery` → `delivered`. `deliveryFailed`, `returned` and `cancelled`
  may come at any time, `readyForPickup` may still become `delivered`, and after
  `deliveryFailed` a new attempt may follow. An event older than the current
  status never changes it.
- The same event twice (same mail, or same `at`, `status` and source) is
  dropped, as is any past 200 per shipment. An account keeps at most 2000
  shipments.
- Delivered, returned and cancelled shipments go 30 days after their status,
  others 60 days after their last news.

## Methods

| Method | |
| --- | --- |
| `Shipment/get`, `Shipment/changes` | as in RFC 8620 |
| `Shipment/query` | filter and sort, below |
| `Shipment/queryChanges` | always `cannotCalculateChanges`; query again |
| `Shipment/set` | create, update, destroy, below |
| `Shipment/rescan` | look back through older mail, below |

Changes come as the data type `Shipment` over EventSource, WebSocket and
[push](jmap-push.md) like `Email`.

### Shipment/query

`filter` (a `FilterCondition` or `FilterOperator` with `AND`, `OR`, `NOT`):

| Condition | |
| --- | --- |
| `status` | a list of statuses |
| `active` | `true`: not delivered, returned or cancelled; `false`: the others |
| `hidden` | `true` or `false` |
| `carrier` | one carrier id |
| `deliveredAfter` | delivered after this UTC date |
| `expectedBefore` | expected to start before this UTC date |

`sort` by `expected` (shipments without a day last) or `updated`; without a sort
the most recently updated come first.

### Shipment/set

- **create** takes `carrier`, `number` and an optional `title`, for a parcel the
  person adds by hand; it starts as `shipped`. The number is normalized; the same carrier and number
  twice is `alreadyExists`, an unknown carrier or a bad number
  `invalidProperties`. The answer has `id`, `trackingUrl`, `status`,
  `statusAt`, `created` and `updated`.
- **update** takes `title`, `hidden` and **`addEvents`**: a list of at most 200
  events with `at` (a UTC date, required), `status` (required), `text` (at most
  500 characters) and `location` (at most 200). Other keys are
  `invalidProperties`. The server sets `source: "client"` and `clientName` from
  the name of the app password or OAuth app the request came with. The rules
  above apply, so writing back the full carrier history again and again is
  harmless.
- **destroy** deletes a shipment. A later mail about the same number creates
  it again.

This is how Home Assistant (or the app) writes the carrier's live status back:
it reads `number` and `carrier`, asks the carrier's API itself and sends what it
learned as `addEvents`.

### Shipment/rescan

```json
["Shipment/rescan", { "accountId": "a1", "days": 14 }, "0"]
```

Looks through the mail of the last `days` (1 to 90, default 14; otherwise
`invalidArguments`), oldest first, in the background. Answers at once:

```json
["Shipment/rescan", { "accountId": "a1", "rescan": { "since": "2026-10-31T23:50:00Z", "startedAt": "2026-11-14T23:50:00Z", "found": 0, "running": true } }, "0"]
```

A mail already noted changes nothing, so a rescan may be repeated. Switching
shipments on starts one over 14 days.

## Switching off

Each person decides with the setting `shipments` (on unless switched off), in
the portal under *My account* and as `values/shipments` (`true`/`false`) in
[`UserSettings`](jmap-settings.md). Off means: nothing is read, nothing is
stored, and **every stored shipment is deleted**. While off, `Shipment/query`
and `get` find nothing, and `Shipment/set` create and `Shipment/rescan` answer
`forbidden`.
