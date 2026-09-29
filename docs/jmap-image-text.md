# JMAP extension: text in pictures

Some mails carry what matters only as a picture: a concert poster, an
invitation, a flyer with the date on it. UwUMail Server reads the text in a
message's pictures with OCR ([Tesseract](https://github.com/tesseract-ocr/tesseract)),
so a client can find dates and places in it like in the text of the mail.

## Capability

`urn:uwumail:jmap:imagetext` in the session's `capabilities` and in the
`accountCapabilities` of mail accounts, including folders others share with
the account:

```json
"urn:uwumail:jmap:imagetext": { "maxImages": 20, "unavailable": false }
```

The capability is there whether or not the server can read pictures;
`unavailable` is `true` while Tesseract is not installed or `[ocr] enabled` is
off ([configuration.md](configuration.md#text-in-pictures-ocr)). A client adds
it to `using` to call `Email/imageText`.

## Email/imageText

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id` | the message whose pictures to read |
| `remote` | `Boolean` | also read the remote pictures of its HTML (default `false`) |

The response:

```json
{
  "accountId": "a7",
  "emailId": "e42",
  "unavailable": false,
  "images": [
    { "source": "cid:poster@example.com", "text": "Premiere: Freitag, 9. Oktober\nKino am Hafen", "width": 1200, "height": 1600 },
    { "source": "https://cdn.example.com/banner.jpg", "text": "Sale ends Sunday", "width": 600, "height": 200 }
  ],
  "skipped": 1
}
```

- `source` says which picture: `cid:<content-id>` for one embedded in the
  message, `blob:<blobId>` for a picture attachment (or an embedded one without
  a `Content-ID`), and the `http`/`https` address for a remote picture.
- `width` and `height` are the picture's own size in pixels.
- Pictures in which nothing was found are left out; `images` is in the order
  the pictures come in the message, embedded ones and attachments first.
- `skipped` counts the pictures that were not read: smaller than 64 pixels on a
  side (icons, spacers, tracking pixels), bigger than 40 megapixels or 10 MB,
  not readable as a picture, beyond the first 20, a remote picture that could
  not be fetched, or one Tesseract failed on or took too long for.
- With `unavailable: true`, `images` is empty and `skipped` is 0.

`notFound` when the message is not there or not visible to the caller;
`invalidArguments` when `remote` is not a boolean.

### Remote pictures

Remote pictures are read only with `remote: true`, which a client sends only
once the person let this message load its pictures: reading them fetches them
and tells their senders the message was opened, like showing them would. They
are then fetched exactly like the reader's pictures ([jmap-remote.md](jmap-remote.md)):
through the server's shared cache and the egress (and its proxy), with the same
limits, counting against the reader's share. Only the `src` of `<img>` tags is
read; backgrounds and `srcset` are not.

## How pictures are read

- A picture is decoded on the server (PNG, JPEG, GIF, WebP), shrunk to at most
  2400 pixels on its longer side and turned grey, and handed to Tesseract as a
  PNG on its standard input: `tesseract stdin stdout -l deu+eng`. No file is
  written for it.
- Tesseract runs as a program of its own with one thread, at most two at once
  on the whole server (decoding waits for the same turn), and is killed after
  20 seconds. What it writes is cut off at 256 KB and the text at 20,000
  characters. The shrinking bounds its memory to a few hundred megabytes.
- One call takes at most 90 seconds; two pictures of a message are read at a
  time.
- What came out is kept by the SHA-256 of the picture and the languages in
  `cache/ocr` of the data directory (left out of backups), at most about
  20,000 results, the oldest going first. A newsletter sent to many people is
  read once.

The Docker image carries Tesseract with German and English (about 40 MB).
Other languages: install their data and set `[ocr] languages`, e.g.
`deu+eng+fra`.
