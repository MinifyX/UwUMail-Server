# Profile pictures

Everyone on the server can have a picture that shows next to their mail. Admins
give one to services, shared mailboxes and groups, and each domain can have a
logo that stands in for its addresses without a picture of their own — the
club's logo for `vorstand@`, say.

Pictures are set under *My account* in the portal, in the webmail's settings
and by the apps over JMAP (`urn:uwumail:jmap:profile`, below). Admins find them
on the page of a service or shared mailbox, in a group's dialog and on the
domain page.

## What the server keeps

Whatever is uploaded — PNG, JPEG, WebP or the first frame of a GIF, up to
10 MB and 8000 × 8000 pixels — is decoded on the server, cut to the square in
its middle, scaled to at most 512 × 512 and written anew: JPEG, or PNG when it
has see-through parts. Nothing of the original file survives besides its
pixels: no camera, no place, no editing history. The portal lets people pick
the square and zoom before uploading; the server cuts again anyway, so a client
that sends the whole photo gets the middle of it.

Pictures live in the database, so they are part of every backup.

## Who sees it

Each account (and each group) chooses:

| | |
| --- | --- |
| **Nobody** (`off`) | The picture is kept, and shown to no one but its owner. |
| **People on this server** (`server`, the default) | Everyone with an account here sees it next to mail from that address, in the webmail and the apps. |
| **Everyone** (`public`) | Other servers can find it too: through Libravatar, and in the `Face:` header if that is switched on. |

An admin can forbid public pictures for the whole server (*Server → Settings →
Profile pictures*) or for one domain (the switch on the domain page). Someone
who chose *Everyone* then counts as *People on this server* until it is
allowed again; their choice is kept.

## Libravatar

The server is a [Libravatar](https://wiki.libravatar.org/) provider for its own
addresses. `GET /avatar/<hash>` on the server's host answers for the MD5 or
SHA-256 of a login address or an alias in lower case, and for groups:

- `s` or `size`: 1 to 512 pixels, 80 when not given. Pictures are only ever
  scaled down.
- `d` or `default`, for addresses without a public picture: `404`, `mm` or `mp`
  (a grey silhouette, also for anything else this server does not draw),
  `blank`, or an `https:` address to be sent to. A plain `http:` address gets
  the silhouette.

Only public pictures are answered, and only where the domain and the server
allow them. A person without a picture of their own gets their domain's logo,
if it has one. Masked addresses and forwarding addresses are never in the
table, so their hashes lead nowhere. One network (a /24, or a /48 for IPv6)
may ask 120 times a minute.

For other servers to find this one, the domain publishes

```
_avatars-sec._tcp.example.org.  SRV  0 1 443 mail.example.org.
```

The DNS check recommends it for every domain that allows public pictures, and
the Cloudflare button can put it in.

The other way round, the server looks up senders from elsewhere the same way:
only where their domain publishes `_avatars-sec._tcp`. It never asks Gravatar
and never falls back to libravatar.org, so a domain that does not say where its
pictures are learns nothing.

## The Face header

With *Send my picture with my mail* switched on, every mail someone sends from
their own address or an alias carries a 48 × 48 copy of their picture as a
[`Face:` header](https://quimby.gnus.org/circus/face/): a PNG with as few
colours as it takes to stay under 725 bytes. Some mail apps show it. The switch
only works while the picture is public, and only for people — never for a
service, a shared mailbox, a group, a masked address or any other address
someone may send as. The header is added before the DKIM signature, so it
arrives signed with the rest. A `Face:` header the mail app wrote itself is
removed, whoever sends: the server decides which picture goes out.

Incoming, the server keeps the Face of a message for its From address when
DMARC passed for the From domain and the message is not junk, and only when it
is a small PNG (at most about 48 × 48 pixels and 2 KB). The newest Face per
address is kept, 20,000 at most; the oldest go first.

## Pictures of senders

What the webmail and the apps show for a sender is described with
[`pictureUrl`](jmap-remote.md#sender-pictures): the reader's own contact photo
first, then the picture of someone here, a Face, Libravatar, the domain's logo,
and a company's logo.

## Privacy

- **What other servers learn.** Nothing, as long as a picture is not public. A
  public picture can be fetched by anyone who knows or guesses the address:
  that is what public means. Libravatar clients ask by a hash of the address,
  but a hash of a known address is easy to make, so the hash hides nothing
  from someone who has the address already.
- **The Face header** goes to everyone who gets the mail, and with it to
  everyone they forward it to. It is off unless switched on.
- **Looking up senders** asks the sender's domain for its SRV record and its
  Libravatar server for the picture. That server learns that someone on this
  server looked, not who or when: answers are kept for a week for the whole
  server, and the request goes through the egress like remote pictures
  ([configuration.md](configuration.md#remote-pictures-through-a-vpn)).
- **Masked addresses** never get a picture and never give one away: no profile
  picture, no domain logo, no Libravatar hash, no Face. Only the reader's own
  address book can put a picture next to one.

## JMAP: `urn:uwumail:jmap:profile`

The capability is in `capabilities` and in the account's
`accountCapabilities`:

```json
"urn:uwumail:jmap:profile": { "maxSize": 10485760, "mayBePublic": true }
```

`mayBePublic` is false while an admin forbids public pictures for the server or
the account's domain; the session state changes with it.

`ProfilePicture` is a singleton with the id `singleton`, like
`VacationResponse`:

| Property | Type | |
| --- | --- | --- |
| `id` | `"singleton"` | |
| `blobId` | `String\|null` | The stored picture, downloadable over `downloadUrl`; `null` without one. |
| `type` | `String\|null` | Its media type. |
| `visibility` | `"off"\|"server"\|"public"` | `server` by default; `public` reads as `server` while forbidden. |
| `sendFace` | `Boolean` | `false` by default; has an effect only while `visibility` is `public`. |
| `updated` | `UTCDate\|null` | When the picture was set. |

`ProfilePicture/get` works as usual. `ProfilePicture/set` takes only an update
of `singleton`; create and destroy fail with `singleton`.

- `blobId` set to an uploaded blob: the picture is decoded and written anew as
  described above; the new `blobId`, `type` and `updated` come back in
  `updated`. A blob that is not a usable picture is `invalidProperties`
  (`blobId`), one larger than `maxSize` `tooLarge`, one that is not there or not
  the caller's `blobNotFound`. Sending the stored `blobId` back changes nothing.
- `blobId: null` removes the picture.
- `visibility: "public"` while it is not allowed is `invalidProperties`
  (`visibility`), as is any other property or value.

`ProfilePicture` has a state of its own and is a push type.

## Tests

`crates/uwumail-smtp/src/profile_pictures.rs` checks decoding, limits, metadata
and the Face PNG; `crates/uwumail-jmap/tests/integration/profile.rs` and
`pictures.rs` the JMAP methods, `pictureUrl` in its order with a stand-in for the
internet, and the Libravatar endpoint; `crates/uwumail-smtp/tests/integration/faces.rs`
the Face header both ways; `crates/uwumail-web/tests/integration/pictures.rs` the
portal.
