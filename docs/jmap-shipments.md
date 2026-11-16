# JMAP extension: shipments

UwUMail Server follows parcels from the mail people get anyway: shipping
confirmations, "out for delivery" notes and pickup notices from shops and
carriers. Each parcel becomes a `Shipment` with its carrier, tracking number,
status and history. The webmail shows them, and apps such as Home Assistant can
read them and write the carrier's live status back.

Everything happens on the server and without AI: the same rules as the
[labels](labels.md#shipments) read the mail when it arrives. **Links in a mail
are never opened** and the server sends no Web Push of its own for parcels.
Shipments are kept per account. If the admin switches on the
[carrier lookup](#carrier-lookup), the server also asks the carriers about a
parcel, by its number only and at fixed addresses, never through a link from a mail.

## Capability

`urn:uwumail:jmap:shipments` in the session's `capabilities` (an empty object)
and in the `accountCapabilities` of the person's own account only:

```json
"urn:uwumail:jmap:shipments": { "maxEvents": 200, "maxRescanDays": 90, "lookupCarriers": ["dhl", "deutschePost"] }
```

| Property | |
| --- | --- |
| `maxEvents` | events one shipment keeps, and that one `addEvents` may bring |
| `maxRescanDays` | the furthest `Shipment/rescan` looks back, in days |
| `lookupCarriers` | the carriers the server asks itself for this account (see [Carrier lookup](#carrier-lookup)); `[]` while the lookup is off for the server or the account |

An app password with the usage `mail` is enough.

### Shared accounts

Each account keeps its own shipments; they are never merged. An account others
share with the person (a shared mailbox such as `info@`, a shared folder) shows
its shipments to whoever may read the mail they came from: `Shipment/get`,
`/changes`, `/query` and `/queryChanges` with the shared account's `accountId`
see a shipment when one of its mails lies in a mailbox the person may read
(`r`), and then only the events and `emailIds` of mails they may read, plus the
events apps added. A shipment someone added by hand goes by the inbox. `Shipment/set`
needs the right to change that mail (`w`): without it `create`, `update` and
`destroy` answer `forbidden`, and a shipment the person cannot see is
`notFound`. `Shipment/rescan` works only on the person's own account
(`accountNotSupportedByMethod`). Limits: no push `StateChange` is sent for
shipments of a shared account, and a shipment whose mails are all deleted is
seen only by the owner.

## Shipment

```json
{
  "id": "s7",
  "carrier": "dhl",
  "number": "00340434000000000000",
  "trackingUrl": "https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434000000000000",
  "shopTrackingUrl": "https://www.shop.example/sendungsverfolgung/?orderNo=12345",
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
| `trackingUrl` | the carrier's own tracking page for the number; for a shipment without a number, a link from the mail on that carrier's own domain; `null` otherwise. Never opened by the server |
| `shopTrackingUrl` | the shop's own tracking page ("Beim Shop ansehen"): only from mail whose sender is authenticated (SPF/DKIM/DMARC), only a link on the From address's registrable domain whose path names tracking, with `utm_*`, `oxm_*` and `mc_*` parameters removed; `null` otherwise. Never opened by the server |
| `title` | the person's own name for the parcel, at most 200 characters |
| `merchant`, `orderRef` | the shop and its order number, as the mail names them |
| `status` | `ordered`, `shipped`, `inTransit`, `outForDelivery`, `delivered`, `readyForPickup`, `deliveryFailed`, `returned` or `cancelled` |
| `statusAt` | when the status was reached |
| `expected` | when the parcel comes, or `null`. With a time window `allDay` is `false` and `to` is `null` when the mail names only a start. A day without times has `allDay: true`, `from` that day's midnight and `to` the next midnight. Times are local to the person's time zone (that of their default calendar, else Europe/Berlin) with their offset |
| `deliveredAt` | when it was delivered, or `null` |
| `pickup` | `{ "kind": "packstation" \| "postOffice" \| "parcelShop" \| "accessPoint", "name", "address", "until" }` (`until` a date, `YYYY-MM-DD`), or `null` |
| `hidden` | the person put it out of sight |
| `emailIds` | the mails that told of it, oldest first |
| `events` | the history, oldest first; `source` is `mail` (with `emailId`), `ai` (with `emailId`, see below), `carrier` (the server asked the carrier) or `client` (with `clientName`) |
| `lookedUpAt` | when the server last asked the carrier, or `null` |
| `lookupError` | how that last question went wrong: `notFound` (the carrier does not know the number), `unavailable` (no answer, a timeout or an error) or `rateLimited`; `null` when it went well or was never asked |

### How mails become shipments

- A mail counts when the rules are sure enough (confidence 0.8, see
  [labels.md](labels.md#shipments)); junk and newsletters about free shipping
  do not.
- The same number of the same carrier (or of any carrier when the mail names
  none) is the same shipment. An order confirmation without a number becomes
  the shipment once the shipping mail with the same order number or from the
  same shop arrives. A shop name that starts the other one counts as the same
  shop („Apple“ and „Apple Store“). Without a number the shop's own tracker is
  kept as `trackingUrl` (Amazon delivering itself); the carrier's page replaces
  it once a number comes. Tickets for events are no shipments.
- **The status only moves forward**: `ordered` → `shipped` → `inTransit` →
  `outForDelivery` → `delivered`. `deliveryFailed`, `returned` and `cancelled`
  may come at any time, `readyForPickup` may still become `delivered`, and after
  `deliveryFailed` a new attempt may follow. An event older than the current
  status never changes it. Mail that is not authenticated (no SPF/DKIM pass
  for the sender's domain) may add news and fill in what is missing, but cannot
  end a shipment that is already known (`delivered`, `returned`, `cancelled`).
- The same event twice (same mail, or same `at`, `status` and source) is
  dropped, as is any past 200 per shipment. An account keeps at most 2000
  shipments; when it is full, a new one replaces the oldest that is done or has
  had no news for 7 days, and is skipped only when there is none.
- Delivered, returned and cancelled shipments go 30 days after their status,
  others 60 days after their last news.
- A day without a year or a relative day ("morgen") is read in the person's
  time zone from the date the mail arrived.
- An event dated after now counts as now for the status and for pruning.
- When a mail is deleted, the `text` of its events (the subject) is cleared;
  the status and time stay.

### The model

When the person has AI labels on and the `shipping` label goes on a mail but the rules could not read
it in full (no number or no status), the label worker asks the model once, in
the same structured form as the assistant's event extraction. Its answer
counts only when it is sure (0.8) and a tracking number or order number in it
appears **word for word** in the mail's subject or text; anything else it names
is left out, and it never sets a tracking link or a pickup place. Such events
have `source: "ai"`. The ask goes through the assistant's usual checks: the
person's permission for AI labels, the provider's daily request and token
limits, and the cost limits. A failed ask never stops the labels.

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
  500 characters) and `location` (at most 200). Other keys, and an `at` more
  than a year ahead, are `invalidProperties`. The server sets `source: "client"` and `clientName` from
  the name of the app password or OAuth app the request came with. The rules
  above apply, so writing back the full carrier history again and again is
  harmless.
  **`removeEvents`** takes back events apps added (`source: "client"`), each
  given by its `at` and `status` as sent; mail events stay. The status is then
  worked out again from the events left. This is the app's „Undo“ after
  marking a parcel by hand.
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

Mail in the Trash is read too (people bin a shop's mail once read), Junk is
not. A mail already noted changes nothing, so a rescan may be repeated. Switching
shipments on starts one over 14 days.

A mail moved to Junk takes its parcel along when the parcel came only from it: one with a title
of its own, hidden, or known from other mail or an app stays and only loses that mail (`updated`
instead of `destroyed`). Fake shipping mail is kept out at delivery too (docs/spam-filter.md,
"Parcel mail").

### Shipment/refresh

```json
["Shipment/refresh", { "accountId": "a1", "ids": ["s7", "s8"] }, "0"]
```

Asks the carriers about these shipments now, and waits for the answers (at
most 10 s altogether). `ids` is required, at most 20 (else `requestTooLarge`).
Needs the right to read the mail (`r`), like `Shipment/get`.

```json
["Shipment/refresh", { "accountId": "a1", "refreshed": ["s7"], "notRefreshed": { "s8": { "type": "tooSoon" } } }, "0"]
```

| `type` | |
| --- | --- |
| `tooSoon` | refreshed less than 5 minutes ago (by anyone with access) |
| `notSupported` | no lookup for this carrier, the lookup is off, there is no number, or the parcel is delivered, returned or cancelled |
| `unavailable` | the carrier did not answer in time, answered with an error, or its rate limit is reached |
| `notFound` | no such shipment, or the carrier does not know the number |

What the carrier said lands in `events`, `status`, `expected` and
`lookedUpAt`/`lookupError` as usual; `Shipment/changes` reports it.

## Carrier lookup

Off by default. An admin switches it on under *Settings → Apps & webmail →
Parcel lookup* in the portal, or in the config file:

```toml
[shipments.lookup]
enabled = true
dhl_key = "…"             # developer.dhl.com, "Shipment Tracking – Unified"; also Deutsche Post
ups_client_id = "…"       # developer.ups.com, OAuth client credentials
ups_client_secret = "…"
fedex_client_id = "…"     # developer.fedex.com, OAuth client credentials
fedex_client_secret = "…"
unofficial = true         # DPD, Hermes and GLS without a key
```

A carrier is asked only once its key is there; DPD, Hermes and GLS have no
official interface, so the server uses the one behind their tracking pages
while `unofficial` is on. Those may stop working at any time; their failures
are only logged. Amazon Logistics and `other` are never asked.

**What leaves the server**: the tracking number and, for the carrier, nothing
else, sent to the carrier's fixed address (never a link from a mail, never the
mail itself). No postcode is stored, so none is sent. With `egress.parcels`
(`[egress] parcels = true`) the questions take the egress proxy, so carriers
see the proxy's address. Operators who switch the lookup on should mention it
in their privacy policy: the carriers learn that this server asks about a
number, and when.

**When**: every 2 hours, every 30 minutes while a parcel is out for delivery,
and only up to 30 days after its last mail. Never again once it is delivered,
returned or cancelled. DHL is asked at most once every 5 s and 250 times a day
(the limits of its free key); every carrier has such a gap. A question that
hits the limit waits for the next round.

**What comes back** becomes events with `source: "carrier"` (an event the
shipment already has is not added again), and the carrier's expected delivery
day becomes `expected`.

**Each account** decides with the setting `shipmentLookup` (on unless
switched off), in the portal under *My account → Parcels* and as
`values/shipmentLookup` in [`UserSettings`](jmap-settings.md). Off, nothing
of that account is asked and `lookupCarriers` is `[]`. The portal learns
whether the lookup is offered at all from `features.shipmentLookup` in
`/api/session`.

### Unknown parcel numbers in doubtful mail

A mail that names a parcel number and is doubtful (the spam filter calls it
suspicious, or it looks like a carrier or shop it does not come from, without
passing SPF/DKIM/DMARC for them) is checked against the carrier while the
lookup is on. Only when the carrier says clearly that it does not know any of
the numbers is the mail moved to Junk and noted in the spam history with
`PARCEL_NUMBER_UNKNOWN`. An error, a timeout or a rate limit changes nothing,
and the unofficial carriers never move a mail.

## Switching off

Each person decides with the setting `shipments` (on unless switched off), in
the portal under *My account* and as `values/shipments` (`true`/`false`) in
[`UserSettings`](jmap-settings.md). Off means: nothing is read, nothing is
stored, and **every stored shipment is deleted**. While off, the account
loses the capability in the session and every `Shipment/*` call answers
`accountNotSupportedByMethod`.

### For the whole server

An admin switches parcels off for everyone with the setting
`shipments.enabled` (on by default) under *Settings → Apps & webmail* in the portal, or
in the config file:

```toml
[shipments]
enabled = false
```

It takes effect at once. The switch that counts for an account is: the server
setting is on **and** the account has not switched `shipments` off. Detection,
rescans, the `Shipment/*` methods, push and shared views all follow it.

Switching the server off does **not** delete stored shipments; they are hidden
and come back as they were when it is switched on again, without a rescan.
Mail that arrives while it is off is not looked at later. Rescans asked for
before wait until it is on again. `values/shipments` in `UserSettings` can
still be written while the server is off.

The session follows the effective switch:

| Server | Account | `capabilities` | the account's `accountCapabilities` | `primaryAccounts` |
|---|---|---|---|---|
| off | either | missing | missing | missing |
| on | off | present | missing | missing |
| on | on | present | present | present |

A shared account carries the capability only while its owner's switch is on.
The session `state` (and `sessionState` in every response) changes whenever
either switch changes. A `Shipment/*` call for an account without the
capability is answered with `accountNotSupportedByMethod`.

The portal learns the server setting from `features.shipments` in
`/api/session` and hides the *Parcels* card while it is `false`.
