# JMAP extension: remote pictures

A picture in a message that lives on the sender's server tells the sender, once
it is loaded, that the message was opened, when, and from which address. UwUMail
Server fetches such pictures on the reader's behalf, so the sender only ever sees
the server — or, with `[egress] proxy` set, a VPN
([configuration.md](configuration.md#remote-pictures-through-a-vpn)).

Nothing in the message is rewritten. Whether its pictures are shown stays the
reader's choice in the client; the client asks the server for each picture once
it may show it, and never loads one from the sender directly.

## Capability

`urn:uwumail:jmap:remote` in the session's `capabilities`:

```json
"urn:uwumail:jmap:remote": {
  "imageUrl": "https://mail.example.com/jmap/image/{accountId}?url={url}",
  "maxSizeImage": 10485760
}
```

`imageUrl` is a URL template like `downloadUrl` (RFC 8620, section 2): the
client fills in `accountId` and the picture's absolute `http` or `https`
address as `url`, percent-encoded as a query value (`encodeURIComponent`).
Addresses relative to the message, `cid:` and `data:` never go here; the client
resolves those itself.

## Fetching a picture

`GET` on the filled-in `imageUrl`, with the same login as every other JMAP
request: `Authorization`, or the webmail's session cookie.

| Answer | When |
| --- | --- |
| `200` | The picture. `Content-Type` is the type it was sent with when that is an `image/*` type, otherwise what its first bytes say (PNG, JPEG, GIF, WebP, ICO, BMP). |
| `400` | Not an `http`/`https` address, longer than 4096 characters, with a login in it, or leading to an address that is not on the open internet — also after a redirect. |
| `401` / `404` | Not logged in, or not one's own account. |
| `502` | The sender's server did not answer, answered with an error or with something that is not a picture, the picture is bigger than `maxSizeImage`, or the egress proxy is away and `fallback` is `block`. |
| `504` | No answer within 20 seconds. |

Up to four redirects are followed. The request to the sender carries no
cookies, no referrer and the agent string `Mozilla/5.0`, nothing that points at
the reader or at this server's software.

A picture comes with `Cache-Control: private, max-age=86400`, so opening the
message again does not ask the sender again. It also comes with
`Content-Disposition: attachment` and a sandboxing `Content-Security-Policy`:
an `<img>` ignores both, while someone who opens the address itself gets a
download instead of a page that could run on the server's origin.

The server fetches at most 32 pictures at a time, for all accounts together.
