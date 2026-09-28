# JMAP push subscriptions: Web Push and UnifiedPush

A mail app that is closed can't hold a connection open to hear about new mail.
Phones and browsers have a push service for that instead: the app hands the
server an address at that service, and the server POSTs a short message there
when something changes. The service wakes the app, which then syncs.

UwUMail Server does this the way JMAP describes it: `PushSubscription` (RFC 8620,
section 7.2), delivered as Web Push (RFC 8030), encrypted for the device
(RFC 8291) and signed with the server's key (VAPID, RFC 8292, announced as in
RFC 9749). The webmail uses it for notifications with the tab closed
([webmail.md](webmail.md)); the Android app can use it through a UnifiedPush
distributor instead of keeping a connection open in the foreground.

For apps that are open, the EventSource (`/jmap/eventsource`) and the WebSocket
(`/jmap/ws`, [jmap-tokens.md](jmap-tokens.md#websocket-rfc-8887)) push the same
`StateChange` directly.

## What leaves the server

Only a `StateChange`: which account changed, which data types changed, and
their new state strings. Never a sender, a subject or a single word of a mail.
The one other push is a `CalendarAlert` for a subscription whose `types` are
`null` or name `CalendarAlert`: which event's alert went off, by ids and uid
(see [jmap-calendars.md](jmap-calendars.md#alerts)).

```json
{
  "@type": "StateChange",
  "changed": { "a1": { "Email": "1204", "EmailDelivery": "1204", "Thread": "1204" } }
}
```

When the subscription came with keys (a browser always gives them), even that
is encrypted, so the push service carries something it can't read. The app
learns what is new by asking the server itself, over its own authenticated
connection. The push service does learn that *something* happened for this
device, and when.

## The server's key

Every push is signed (`Authorization: vapid t=…, k=…`), with a JWT for the push
service's origin, valid for 12 hours, whose `sub` is `https://` and the server's
hostname. Browsers only accept pushes signed with the key a subscription was
made for, so the session names it:

```json
"urn:ietf:params:jmap:webpush-vapid": {
  "applicationServerKey": "BK9…"
}
```

The key is a P-256 key pair made on first use and kept sealed in the settings
(`push.vapid_key`), like the passwords of fetched mailboxes. It never changes on
its own; a restored database brings its key along.

## PushSubscription

The methods belong to `urn:ietf:params:jmap:core` and, unlike every other
method, take no `accountId` and have no state: a subscription belongs to the
login that made it, not to an account.

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | server-set (`w12`) |
| `deviceClientId` | `String` | the client's name for this device, at most 255 characters |
| `url` | `String` | the push service address; never returned |
| `keys` | `{ p256dh, auth }` or `null` | the device's P-256 key and 16-byte auth secret, base64url; never returned |
| `verificationCode` | `String` or `null` | `null` until the client sent back the right code, then that code |
| `expires` | `UTCDate` | at most 7 days ahead; `null` or nothing on create means 7 days |
| `types` | `String[]` or `null` | the types to push; `null` means all |

### Creating one

```json
["PushSubscription/set", { "create": { "k": {
  "deviceClientId": "4f0c…",
  "url": "https://push.example.net/wpush/v2/gAAAA…",
  "keys": { "p256dh": "BCVx…", "auth": "BTBZ…" },
  "types": ["EmailDelivery"]
} } }, "0"]
```

The answer has the `id` and the `expires` the server chose. Right away the
server pushes a `PushVerification` to the address:

```json
{ "@type": "PushVerification", "pushSubscriptionId": "w12", "verificationCode": "3f9a…" }
```

Nothing else is ever sent to the address until the client updates the
subscription with that code:

```json
["PushSubscription/set", { "update": { "w12": { "verificationCode": "3f9a…" } } }, "0"]
```

A wrong code is `invalidProperties`; after five wrong ones the subscription is
gone. The code may arrive before the answer to the create does. A verification
that doesn't arrive is not sent again; an unverified subscription is dropped
after a day.

### Keeping it

A subscription lasts at most a week. The client extends it by updating
`expires` (the server shortens anything further away and says so in `updated`),
e.g. every time the app starts. `types` can change too; `url`, `keys` and
`deviceClientId` can't: destroy the subscription and create another.

`PushSubscription/get` lists the subscriptions of the login it is called with.
Asking for `url` or `keys` is `forbidden`.

```json
["PushSubscription/get", { "ids": null }, "0"]
→ ["PushSubscription/get", { "list": [{ "id": "w12", "deviceClientId": "4f0c…",
     "verificationCode": "3f9a…", "expires": "2026-10-04T16:21:29Z",
     "types": ["EmailDelivery"] }], "notFound": [] }, "0"]
```

### Rules

- The address has to be `https://` and reach a public address, checked when it
  is subscribed and again when it is resolved, the same way as remote pictures;
  redirects are not followed.
- At most 50 subscriptions per account, and 30 new ones per account and hour.
- A subscription belongs to the login that made it and is only listed for that
  login: the webmail's session cookie, an app password (as a bearer token or in
  Basic), an app signed in with [OAuth](oauth.md), or the account password. It
  ends with it: with the session when someone signs out or it runs out, when
  the app password is removed or expires, when the OAuth app is signed out, and
  for the account password when the password, a second factor or
  the person's app password rule changes. From then on nothing is pushed to it,
  and it is dropped with the next hourly cleanup. An account that is switched
  off or deleted gets no pushes.
- Expired subscriptions, and those never verified within a day, are dropped
  every hour.

## When pushes go out

Changes are collected for two seconds, and a subscription gets at most one
push every five seconds; what changes in between is sent along with the next.
Changes in folders others share with the account arrive as changes of their
shared account, as over the EventSource.

`EmailDelivery` is only in a push when new mail arrived: a message that is
neither read nor a draft and did not land in the drafts, sent, junk or trash
folder. A message marked as read or moved, a draft the app saved and the copy
it filed in Sent are changes of `Email`, but no delivery; neither is mail to a
disabled masked address, which goes into the Trash
([jmap-masked-email.md](jmap-masked-email.md)). In a shared account, only
those who may read the folder the mail came into hear of the delivery: people
a folder was shared with, and every member of a shared mailbox
([groups.md](groups.md)). A
browser has to show something for every push it gets, so the webmail asks for
`EmailDelivery` alone.

Each push carries `TTL` (12 hours: whatever is older, the next sync finds
anyway), `Urgency: high` for new mail and `normal` otherwise, and
`Topic: jmap-state`, so a newer change replaces one the push service still
holds for a device that is away. Verifications go out with `Urgency: high` and
without a topic.

How the push service answers decides what happens next:

| Answer | What happens |
| --- | --- |
| `2xx` | Fine; failures are forgotten. |
| `404`, `410` | The push service forgot the subscription, so the server does too, also when it answers the verification like that. |
| `429`, `5xx`, anything else, no answer | The next push waits: half a minute, doubling after each failure, at most an hour. After 20 failures in a row the subscription is dropped. |

A message is at most 4096 bytes, encryption included, which is what every
push service takes.

## The webmail

The webmail ([webmail.md](webmail.md)) signs in with the portal's session
cookie, so its subscription belongs to that session. Its service worker is
served at `/mail/sw.js` with `Cache-Control: no-cache` (a new webmail's worker
is picked up at the next visit) and a Content-Security-Policy of its own that
lets it talk to this server only (`connect-src 'self'`, `img-src 'self'`). Its
scope is all of `/mail/`, which needs no `Service-Worker-Allowed` header.

When a push arrives, the worker has no page to ask, so it asks the server
itself: `GET /api/session` with the cookie for the session's CSRF token, then
`POST /jmap/api` with the cookie and `X-CSRF-Token` for `Mailbox/query` (the
inbox), `Email/query` (unread since last time) and `Email/get` (sender and
subject), and shows a notification. A verification code it sends back the same
way. Once the session is gone, `/api/session` answers `null` and the API 401,
and the worker takes its push subscription down.

## UnifiedPush

A UnifiedPush distributor on Android (ntfy, NextPush, Sunup and others) hands the
app an address like any Web Push service, and the app subscribes it the same
way. Distributors that offer Web Push (with `keys`) get encrypted pushes; for
those without, the subscription is created without `keys` and the server sends
the plain JSON above, which still names nothing but types and states. The
address still has to be `https://`; a distributor on a local network address
can't be reached, since the server only pushes to public addresses.

## Storage

Migration 0046 adds the table `push_subscriptions`. The address and the auth
secret are sealed; the log names only the push service's host. When a
subscription is destroyed, its row is deleted.
