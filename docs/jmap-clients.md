# JMAP: third-party clients

UwUMail speaks plain JMAP (RFC 8620, RFC 8621, RFC 8887), so mail programs other than the UwUMail
app and the webmail can use it. This page says how to connect them and what was tried with 0.12.

## Connecting

| | |
| --- | --- |
| Session URL | `https://mail.example.com/.well-known/jmap` (or `/jmap/session`) |
| WebSocket | `wss://mail.example.com/jmap/ws`, subprotocol `jmap` ([jmap-tokens.md](jmap-tokens.md#websocket-rfc-8887)) |
| Push | EventSource from the session's `eventSourceUrl`, or over the WebSocket; with the app closed, Web Push or UnifiedPush through `PushSubscription` ([jmap-push.md](jmap-push.md)) |

Two ways to sign in ([jmap-tokens.md](jmap-tokens.md)):

- **Basic:** `Authorization: Basic base64(login:password)`. The account password works until the
  person has two-factor authentication (or chose "mail apps need app passwords"); an app password
  works always.
- **Bearer:** `Authorization: Bearer <token>`, where the token is an app password made in the
  portal (*My account → Security → App passwords*, use "mail") or one a program gets for itself:

  ```bash
  curl -s https://mail.example.com/jmap/token -H 'content-type: application/json' \
    -d '{"username": "nyu@example.com", "password": "…", "name": "aerc on the laptop"}'
  ```

  The answer has `token` and `sessionUrl`; the token is shown only once.

The session's URLs follow the host and port the client used, over HTTP/1.1 and HTTP/2 alike.
Folders others share with the person are further accounts in the session (not the primary one);
programs that only look at `primaryAccounts` do not see them ([sharing.md](sharing.md)).

### Limits

- A request to `/jmap/api` may carry up to 10 MB, an upload up to 50 MB (`maxSizeRequest` and
  `maxSizeUpload` in the session). The server reads neither before the login was checked; without
  one the answer is `401` at once.
- Uploads are kept for a day, so an email can be made from them, and are not part of the mailbox
  until then. Together, one account's uploads of the last 24 hours may take up to 1 GiB, and never
  more than the account's storage quota when it has one. An upload beyond that is answered with
  `413`; older uploads make room again as they reach their day. Uploading the same file again takes
  no more room.
- Mail methods have the bounds calendars and contacts have: `Email/parse` and `SearchSnippet/get`
  take at most 500 ids (`maxObjectsInGet`, else `requestTooLarge`); a query filter (`Email/query`,
  `Mailbox/query`, `SearchSnippet/get`) at most 100 operators and conditions (`unsupportedFilter`);
  a sort at most 10 comparators (`unsupportedSort`). Parsing messages for `Email/get` and
  `Email/parse` shares the request's 15 seconds with calendar and contact work; once they are used
  up the call answers `serverUnavailable`, and the rest can be asked for in a new request.

## aerc

[aerc](https://aerc-mail.org) has a JMAP backend (`aerc-jmap(5)`) built on the go-jmap library.
Tried with aerc from git (September 2026, go-jmap 0.5.3), with Basic and with a Bearer token,
both as the real program and with a small Go program making the same calls.

```ini
# ~/.config/aerc/accounts.conf — the @ in the login is %40
[UwUMail]
source   = jmap://nyu%40example.com:PASSWORD@mail.example.com/.well-known/jmap
outgoing = jmap://
from     = Nyu <nyu@example.com>
copy-to  = Sent

[UwUMail with a token]
source   = jmap+oauthbearer://:abcd-efgh-jkmn-pqrs@mail.example.com/jmap/session
outgoing = jmap://
from     = Nyu <nyu@example.com>
```

What works: folders, reading (headers, body parts, attachments), threads, flags, moving, copying,
archiving, deleting, creating and removing folders, search, push over EventSource, sending over
JMAP (upload, Email/import into Drafts, EmailSubmission/set moving it to Sent), `copy-to` and
postponing drafts.

Known limitations:

- **Header search** (`:search -H …`) is answered with `unsupportedFilter`: UwUMail keeps no index
  of arbitrary header fields. Searching by subject, from, to, cc, body and flags works.
- aerc may log "Unexpected negative value (-1) for Exists" after a change. Email/queryChanges
  reports every changed email as removed and adds back those still in the results, as RFC 8620
  allows; aerc counts the extra removals. The folder list is right again after the next refresh.
- aerc uses only the primary account, so folders others share are not shown.
- UwUMail up to 0.11 answered uploads with `201 Created`, which go-jmap takes for an error, so
  sending and `copy-to` failed there. With those versions use `outgoing = smtp://…@mail.example.com:587`
  and leave out `copy-to`.

## Fastmail JMAP-TestSuite

[JMAP-TestSuite](https://github.com/fastmail/JMAP-TestSuite) (Perl) was run against a local server
with an adapter that makes a fresh account per "pristine" test with `uwumail-server account add`,
over HTTP and over the WebSocket (the results are the same). It fixed Mailbox/query, Mailbox/set
and a number of Email/get and Email/set details in 0.12.

It was written against drafts of the JMAP specifications and against Cyrus, so many of the tests
that still fail check things RFC 8620/8621 do not ask for, or ask for differently:

| Failing because the suite expects… | RFC |
| --- | --- |
| no `accountId` in method calls (the adapter adds it) | required |
| error objects without `description`, or with a non-standard `arguments` | `description` is allowed |
| `total` in /query without `calculateTotal`, `filter`/`sort` echoed in /queryChanges | not in the RFC |
| the session at the API URL (`GET /jmap/api`) | the session has its own URL |
| a download template without `{type}` | `{type}` is required |
| `onDestroyRemoveMessages` | `onDestroyRemoveEmails` |
| `notCreated: {}` rather than `null` | both allowed |
| new folders with `isSubscribed: false` and `sortOrder: 10`, a new account with only an Inbox | server's choice |
| `us-ascii` for text the server built from `bodyValues`, `size: 0` for multipart parts | server's choice / not defined |
| `subParts: []` for leaf parts, `position: 0` past the end | `null` / not defined |
| case-sensitive sorting of folder names | collation is the server's |

Missing optional features: `updatedProperties` in Mailbox/changes (always `null`, as allowed),
the `header` Email/query filter, and NFC normalisation of `asText` header values.

## Other clients

- **UwUMail app and webmail:** the first-party clients; the webmail's end-to-end test
  (`src/backend/jmap/server.e2e.test.ts` in UwUMail-Webmail) runs against a local server with
  `UWUMAIL_TEST_SERVER=http://127.0.0.1:18080`, and the app's with `dev/client-compat.sh`.
- Clients that speak JMAP but not every extension simply ignore the capabilities they do not
  know (`urn:uwumail:*`, calendars, contacts, Sieve).
