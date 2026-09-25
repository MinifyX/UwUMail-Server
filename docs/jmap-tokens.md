# JMAP: tokens and WebSocket

How programs sign in to JMAP, how they get a token of their own, and how they
talk to the server over one WebSocket instead of separate HTTP requests.

## Signing in

Every JMAP endpoint — the session (`/.well-known/jmap`, `/jmap/session`), the
API, upload, download, the EventSource, the WebSocket, and the picture
endpoints — accepts:

| `Authorization` | What it carries |
| --- | --- |
| `Basic base64(login:password)` | The account password, or an app password |
| `Bearer <app password>` | An app password on its own |

A bearer token is an ordinary app password (My account → Security → App
passwords) that may be used for mail (the `mail` use, which covers JMAP and
IMAP). It is sent as it was shown — `abcd-efgh-jkmn-pqrs` — or without the
dashes, in any case. The password names its account, so there is no login to
send with it. It stops working when it expires, is revoked, when the account
may no longer log in, or when an admin switches JMAP off for the account.

Wrong passwords and wrong tokens count against the client's network together:
after 10 failures in 15 minutes (per IPv4 address or IPv6 /64) every login from
there is answered with `429` until the window has passed.

Once a person has two-factor authentication (or chose "mail apps need app
passwords"), the account password no longer opens JMAP for programs: they
need an app password, from the portal or from the token endpoint below. The
webmail signs in with the portal's session instead (see
[webmail.md](webmail.md)).

Taken-over app passwords from another server (mailcow import) only have a hash
that needs the login to be found, so they work with Basic only.

## Getting a token: `POST /jmap/token`

A program that has the person's login and password can trade them for an app
password of its own, instead of keeping the password. The program names it —
that name is what the person sees in the list of app passwords, where they can
revoke it like any other.

```http
POST /jmap/token HTTP/1.1
Host: mail.example.com
Content-Type: application/json

{ "username": "nyu@example.com", "password": "…", "name": "Mail on the laptop" }
```

| Field | | |
| --- | --- | --- |
| `username` | required | the login |
| `password` | required | the account password; app passwords are not accepted here |
| `name` | required | 1 to 60 characters |
| `code` | with two-factor authentication | six digits from the authenticator app, or a recovery code |
| `expiresInDays` | optional | 1 to 3650; without it the token does not expire |

Unknown fields are refused. A successful answer is `201 Created`:

```json
{
  "token": "abcd-efgh-jkmn-pqrs",
  "tokenType": "Bearer",
  "id": 17,
  "name": "Mail on the laptop",
  "expiresAt": null,
  "accountId": "a3",
  "username": "nyu@example.com",
  "sessionUrl": "https://mail.example.com/jmap/session"
}
```

The token is shown only this once. From then on the program sends
`Authorization: Bearer abcd-efgh-jkmn-pqrs`.

Errors are `application/problem+json` with a `type` of
`urn:uwumail:jmap:token:<kind>`:

| Status | `kind` | When |
| --- | --- | --- |
| 400 | `invalidRequest`, `invalidName`, `invalidExpiry` | the body is not as above |
| 401 | `invalidCredentials` | wrong login or password (counts as a failed login) |
| 401 | `secondFactorRequired` | the password was right, the account has two-factor authentication and no `code` was sent: ask for it and send everything again |
| 401 | `invalidCode` | the code is wrong or was used before (counts as a failed login, and ten wrong codes lock the account's codes here for 15 minutes) |
| 403 | `protocolOff` | the account may not use JMAP |
| 409 | `tooManyAppPasswords` | the account already has 50 app passwords |
| 429 | `tooManyAttempts` | too many failed logins from this network, or too many wrong codes for this account |

The person is told like for an app password made in the portal: a mail
("a new app password was created") and entries in the activity list, one for
the login (`protocol: jmapToken`) and one for the new app password.

A passkey is no code: someone whose only second factor is a passkey uses a
recovery code here, or makes the app password in the portal.

## WebSocket (RFC 8887)

The session advertises the endpoint:

```json
"urn:ietf:params:jmap:websocket": { "url": "wss://mail.example.com/jmap/ws", "supportsPush": true }
```

The URL follows the scheme the client used (`ws://` over plain HTTP). The
handshake is a `GET` with `Upgrade: websocket` (or HTTP/2 `CONNECT`, RFC 8441)
and must ask for the subprotocol `jmap` (`Sec-WebSocket-Protocol: jmap`);
without it the answer is `400`. Programs authenticate the handshake with Basic
or Bearer like any other request.

The webmail cannot set headers on a browser WebSocket, so it uses the portal's
session cookie instead. Then the handshake must come from the server's own
origin (`Origin` equal to the host) and carry the CSRF token as a query
parameter: `wss://mail.example.com/jmap/ws?csrf=<token>`.

Messages are JSON text frames, handled in the order they arrive:

- `{"@type": "Request", "id": "r1", "using": […], "methodCalls": […]}` — a
  JMAP request, answered with `{"@type": "Response", "requestId": "r1", …}`
  (the usual response object). A request that fails as a whole is answered
  with `{"@type": "RequestError", "requestId": "r1", "type": …, "status": …,
  "detail": …}`. Each request checks the account again; one that may no longer
  log in ends the connection.
- `{"@type": "WebSocketPushEnable", "dataTypes": ["Email", "Mailbox"] | null,
  "pushState": "…"}` — from now on, changes of those types (all with `null`)
  arrive as `{"@type": "StateChange", "changed": {"a3": {"Email": "…"}},
  "pushState": "…"}`. With a `pushState` from an earlier `StateChange`,
  whatever changed since is pushed at once.
- `{"@type": "WebSocketPushDisable"}` — no more pushes.

The data types are those of the EventSource: `Mailbox`, `Email`,
`EmailDelivery`, `Thread`, `Identity`, `EmailSubmission`, `VacationResponse`,
`UserSettings`, `Calendar`, `CalendarEvent`, `ParticipantIdentity`,
`AddressBook`, `ContactCard`, `SieveScript`. A single message may be as large as
`maxSizeRequest` (10 MB).
