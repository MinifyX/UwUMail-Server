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
  "pictureUrl": "https://mail.example.com/jmap/picture/{accountId}?email={email}",
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

## Sender pictures

`GET` on the filled-in `pictureUrl` (`email` percent-encoded) answers with the
logo or website icon of a company sender, or `404` when there is none. Only the
registrable domain of the address is asked (`news.mail.shop.de` becomes
`shop.de`), and never for addresses at mail providers such as gmail.com or
web.de, which belong to people.

The server looks, in this order, for:

1. the SVG logo of a BIMI record (`default._bimi.<domain>` in DNS),
2. the icons the website's start page links (`apple-touch-icon` first, then the
   largest `icon`), at `https://<domain>/` or `https://www.<domain>/`,
3. `/favicon.ico`.

The DNS lookup goes out directly; the web requests take the same way as remote
pictures, through the egress proxy when one is set. The answer carries the
picture with its type and `X-Picture-Kind`: `logo` for a picture made to fill a
circle, `icon` for a small symbol that wants a plain background around it.

What was found, and that nothing was, is kept in memory for a week and shared by
all accounts, so a company sees at most one request a week from the server, no
matter who reads its mail and how often. After a restart, or once the week is
over, the server asks again, so a new logo arrives within a week.
