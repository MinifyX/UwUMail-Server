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
  "imageSizesUrl": "https://mail.example.com/jmap/image/{accountId}/sizes",
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
| `200` | The picture. `Content-Type` is what its first bytes show for PNG, JPEG, GIF, WebP, AVIF, ICO and BMP, whatever it was sent as; `image/svg+xml` only for markup that is an SVG and was sent as one (or as XML, or without a type); another `image/*` type as sent, unless the bytes are markup. Anything else is not passed on. `X-Image-Width` and `X-Image-Height` give its size in pixels where it is known. |
| `400` | Not an `http`/`https` address, longer than 4096 characters, with a login in it, or leading to an address that is not on the open internet — also after a redirect. |
| `401` / `404` | Not logged in, or not one's own account. |
| `429` | The reader has 400 pictures waiting already. |
| `502` | The sender's server did not answer, answered with an error or with something that is not a picture, the picture is bigger than `maxSizeImage`, or the egress proxy is away and `fallback` is `block`. |
| `504` | No answer in time (below). |

Up to four redirects are followed. The request to the sender carries no
cookies, no referrer and the agent string `Mozilla/5.0`, nothing that points at
the reader or at this server's software.

A picture comes with `Cache-Control: private, max-age=86400`, so opening the
message again does not even ask the server. It also comes with
`Content-Disposition: attachment` and a sandboxing `Content-Security-Policy`:
an `<img>` ignores both, while someone who opens the address itself gets a
download instead of a page that could run on the server's origin.

### Patience

A newsletter's tracking pixel on a host that is gone must not hold up the rest
of the message, so remote pictures give up much sooner than other requests of
the server:

| Step | Limit |
| --- | --- |
| Connecting, per address (through the proxy: reaching it and the tunnel) | 4 s |
| From the start to the answer's headers, redirects included | 6 s |
| Pause between two pieces of the picture | 5 s |
| Everything, for a big picture on a slow line that keeps coming | 20 s |

A picture that could not be fetched is not asked for again for a minute when
its host did not answer or could not be reached (that may be the proxy's fault
for a moment), and for ten minutes when it is not there or no picture.

### Fair shares

Each person has 8 pictures on their way at a time and up to 400 waiting; all
people together have 64 on their way. One reader's newsletter with a hundred
pictures therefore neither floods the server nor makes anyone else wait, and
pictures take none of the 32 places the server's other requests (sender logos,
push, one-click unsubscribing) share.

The connection to a picture's host is kept for a minute and speaks HTTP/2
where the host does, so the pictures of a message from one host share one
connection, and one tunnel through the proxy.

### Cache

A picture is fetched once for the whole server and kept on disk, in
`cache/images` of the data directory, which backups leave out:

- An entry is found by the SHA-256 of the picture's address alone and holds the
  picture, its checked type and its size, nothing about who asked for it.
  Addresses that carry a recipient's tracking token are different addresses,
  so only pictures that really are the same for everyone are shared.
- Only pictures are kept: what passed the type check above, never an error.
- An entry is fetched again after 7 days at the latest.
- The cache keeps within `egress.image_cache_mb` (1024 by default; admin panel
  → VPN & proxy): once it is full, the entries asked for least lately go first,
  until 90 % of it are used. `0` keeps nothing; one picture never takes more
  than a quarter of it.

Readers who ask for a picture that is already on its way wait for that same
request (single flight): a newsletter to five hundred people, opened at the
same moment, reaches its sender once.

Someone on the server who knows a picture's exact address could tell from how
fast it comes whether someone else here loaded it lately. Tracking addresses
are different per recipient, so this tells nothing about a person.

## Sizes before the pictures

The reader lays a message out before its pictures arrive; for that it needs
their sizes. `POST` on the filled-in `imageSizesUrl`, with the same login as
every other JMAP request (the webmail sends its CSRF token too), and a body of
at most 200 addresses:

```json
{ "urls": ["https://cdn.example.com/hero.jpg", "https://tracking.example.net/open.gif"] }
```

The answer is `application/x-ndjson`: one line of JSON per address, each as
soon as it is known, in the order they come; the last line ends the answer.

```json
{"url":"https://cdn.example.com/hero.jpg","width":1200,"height":600}
{"url":"https://tracking.example.net/open.gif","failed":true}
```

`width` and `height` are `null` when the picture came but its size can't be
read here (AVIF, some SVGs). A size is read from the first bytes of PNG, JPEG,
GIF and WebP (as they arrive, up to 512 KB) and from an SVG's `width`/`height`
or `viewBox`. Asking fetches the pictures into the cache, and goes on after the
size is told, so each picture itself is there right after. `400` for more than
200 addresses or a body that is not like the above; the addresses count against
the person's share like pictures.

## HTTP/2 to the browser

The server speaks HTTP/2 wherever it terminates TLS itself (ALPN `h2`), so a
browser asks for all pictures of a message over one connection instead of six
at a time. Behind a reverse proxy it is the proxy's part: Caddy and nginx speak
HTTP/2 to browsers by default. Nothing needs to be switched on.

## Sender pictures

`GET` on the filled-in `pictureUrl` (`email` percent-encoded) answers with the
picture that stands for one address, as the signed-in account sees it, or
`404` when there is none. The server looks, in this order:

1. **The reader's own contact photo**: the first card with a photo and this
   address (the default address book first, then the others, then those shared
   with the reader). A photo carried in the card (`data:`, or vCard 3 base64)
   is handed out as it is when it is a PNG, JPEG, GIF or WebP; one that is only
   an `https:` link is fetched like a remote picture (same guards, egress and
   size limit, 20 seconds) and kept a day for that account.
2. **Someone on this server**: a login, alias (with or without a `+tag`),
   service, shared mailbox or group whose picture its owner lets people here
   see ([profile-pictures.md](profile-pictures.md)).
3. **A Face** that came with mail from the address, under a DKIM signature of
   its domain that covers the `Face` header; only for addresses of other
   servers.
4. **Libravatar**, for addresses of other servers whose domain publishes
   `_avatars-sec._tcp.<domain>`: `https://<target>:<port>/avatar/<sha256>?s=128&d=404`,
   through the egress. Found or not, the answer is kept a week for the whole
   server. Never Gravatar, never libravatar.org.
5. **The logo of one of our domains**, for its addresses.
6. **A company's logo or website icon** (below).

Masked addresses of this server get nothing past step 1, not even the domain's
logo.

The cards are found through an index of addresses of cards with a photo, kept
with every write over CardDAV and JMAP, so a message list asking for fifty
senders reads fifty rows, not every vCard.

Two query parameters narrow it, appended to the filled-in template:

- `source=logo`: only steps 5 and 6, for "take the company logo".
- `local=1`: nothing that needs a request to another server: contact photos
  carried in the card, people here, Faces, local logos and whatever is already
  known from earlier. For readers who switched sender pictures off.

The answer carries `X-Picture-Kind`: `photo` for a person's picture (steps 1
to 4), which fills the circle; `logo` for a picture made to fill a circle
(domain logos, BIMI logos, app icons); `icon` for a small symbol that wants a
plain background around it. A person's picture comes with
`Cache-Control: private, no-cache` and an `ETag`, so it is asked again each
time and answered `304` while it did not change; logos keep
`private, max-age=86400`, and `404` is `private, max-age=3600`.

### Company logos

Only the registrable domain of the address is asked (`news.mail.shop.example`
becomes `shop.example`), and never for addresses at mail providers such as
gmail.com or web.de, which belong to people. The server looks for:

1. the SVG logo of a BIMI record (`default._bimi.<domain>` in DNS),
2. the icons the website's start page links (`apple-touch-icon` first, then the
   largest `icon`), at `https://<domain>/` or `https://www.<domain>/`,
3. `/favicon.ico`.

The DNS lookup goes out directly; the web requests take the same way as remote
pictures, through the egress proxy when one is set.

What was found, and that nothing was, is kept in memory for a week and shared by
all accounts, so a company sees at most one request a week from the server, no
matter who reads its mail and how often. After a restart, or once the week is
over, the server asks again, so a new logo arrives within a week.

## IMAP accounts

The UwUMail apps use this for IMAP accounts on a UwUMail server too. They know
such a server by its IMAP greeting (`* OK [CAPABILITY …] UwUMail IMAP ready`),
and only then sign in over JMAP, at `/.well-known/jmap` on the IMAP host, just
to have pictures fetched. The greeting is therefore kept as it is; the IMAP
tests check it.
