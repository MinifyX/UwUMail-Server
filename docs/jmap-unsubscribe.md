# JMAP extension: one-click unsubscribe

Newsletters that follow RFC 8058 let a reader leave with one click: the mail
names an `https` link in `List-Unsubscribe` and says, in
`List-Unsubscribe-Post: List-Unsubscribe=One-Click`, that a single POST there is
enough. UwUMail Server sends that POST itself, so the reader's browser never
talks to the newsletter, and the newsletter sees the server — or, with
`[egress] proxy` set, a VPN — exactly as with remote pictures
([jmap-remote.md](jmap-remote.md)).

It is the one place where a header of a received message decides where the
server sends a request. That is why it only happens when the newsletter vouched
for the header with a DKIM signature, only to public addresses, and only once
in a while per message.

## Capability

`urn:uwumail:jmap:unsubscribe` in the session's `capabilities` and in the
`accountCapabilities` of mail accounts, an empty object:

```json
"urn:uwumail:jmap:unsubscribe": {}
```

A client adds the capability to `using` to call `Email/unsubscribe`. Folders
others share with the account appear as accounts with the capability only when
the reader may change something there (the *write* level or more); with *read*
they don't. The method itself asks for the right to set keywords (`w`,
`maySetKeywords`) in a mailbox that holds the message.

## Email/unsubscribe

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id` | the message whose newsletter to leave |

The response is `{ "accountId", "emailId" }` once the newsletter answered with
a `2xx` status. Clicking again within five minutes answers the same without
asking the newsletter a second time.

```json
["Email/unsubscribe", { "accountId": "a1", "emailId": "e42" }, "0"]
```

Method errors:

| `type` | When |
| --- | --- |
| `notFound` | No such email in the account, or none the reader may see. An email id of another account is never found. |
| `cannotUnsubscribe` | The message offers no one-click unsubscribe the server will follow (see below). Nothing was sent; the client falls back to the unsubscribe mail or the web page. |
| `unsubscribeFailed` | It was tried and did not work, or may not be tried right now; `description` says why: the link does not lead to a public address, no answer within 20 seconds, an answer other than `2xx`, the egress proxy is away while `fallback` is `block`, or one of the limits below. |
| `forbidden` | In a shared account: the reader may read the message but not change it, so may not unsubscribe for its owner. |

### When the server follows the link

All of this has to hold, otherwise the answer is `cannotUnsubscribe`:

- The message has exactly one `List-Unsubscribe` and exactly one
  `List-Unsubscribe-Post` header. DKIM covers the lowest header of a name, so a
  second one written above it on the way would go unsigned.
- `List-Unsubscribe-Post` is `List-Unsubscribe=One-Click`.
- `List-Unsubscribe` has an `https:` link in angle brackets (the first one is
  taken; `mailto:` and `http:` ones are left to the client), at most 2048
  characters long.
- A DKIM signature on the message holds and its `h=` tag lists both
  `List-Unsubscribe` and `List-Unsubscribe-Post` (RFC 8058, section 4). The
  server checks the stored message again at the moment of the click, against
  the key in DNS now: the `Authentication-Results` written on delivery do not
  say which headers a signature covered. A key the newsletter has since
  withdrawn means `cannotUnsubscribe`. The signing domain need not be the From
  domain; RFC 8058 does not ask for it, and the link belongs to whoever signed.

### The request

`POST` to the link with the body `List-Unsubscribe=One-Click` as
`application/x-www-form-urlencoded`, the agent string `Mozilla/5.0`, and no
cookies, no referrer, no login — nothing about the reader or this server's
software (RFC 8058, section 3.1).

It takes the way of remote pictures: through the egress proxy when pictures
take it (`egress.pictures`), never around it while `egress.fallback` is
`block` ([configuration.md](configuration.md#remote-pictures-through-a-vpn)).
The link must be `https`, without a login, and its name must resolve to public
addresses only; the server resolves it itself and connects to the checked
address. An address in a private or reserved network is refused before
anything is sent.

Redirects are not followed. RFC 8058 forbids the newsletter to answer with one,
and a POST that follows a redirect often arrives as a GET or somewhere else; a
`3xx` answer is `unsubscribeFailed`. The server waits at most 20 seconds and
reads at most 16 KB of the answer, which it throws away.

### Limits

- Each email is sent at most once in five minutes, whatever came of it. After
  a failure the client can offer the fallback right away.
- Each login may send 30 unsubscriptions an hour, in its own and in shared
  accounts together.
- The server sends at most 32 such requests and pictures at a time, for all
  accounts together.

The limits are kept in memory; a restart forgets them.

### Log

Each unsubscription is logged at `info` with the account, the email id and the
link's host — never the whole link, which often carries a token that
unsubscribes whoever holds it.
