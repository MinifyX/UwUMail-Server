# Signatures per domain and company signatures

Since 0.22 a person writes **one signature per domain** instead of one per address, and an admin
can give a domain a **company signature**: a template, or a footer the server appends to every
message sent from the domain.

## What an address sends with

Every sending address (JMAP `Identity`) resolves its signature like this, first match wins:

1. its own signature (an *override*, set for this one address),
2. the person's signature for the address's domain,
3. the person's signature for every domain (`*`, "Alle Domains"),
4. the domain's company signature, when the admin set it as **template**.

JMAP `Identity/get` returns that **effective** signature in `textSignature`/`htmlSignature`, with
the placeholders filled, so every mail program (UwUMail apps, Thunderbird, other JMAP clients)
keeps working without knowing about domains. `Identity/set` with a signature makes it the
address's own; writing back exactly what `Identity/get` returned changes nothing (a client saving
all identities does not freeze a copy of the domain's signature). A new identity created with an
empty signature takes its domain's.

## Placeholders

`{name}`, `{adresse}` (also `{address}`, `{email}`) and `{domain}`, case-insensitive, filled per
address: the identity's name (else the account's display name), its address and the address's
domain. Unknown braces stay as they are. In HTML the values are escaped, so a name like
`<b>Mini</b>` shows as text and never becomes markup.

## Where to edit

- **Portal**, My mailbox → Sending: pick a domain at the top (with the number of addresses),
  edit one signature for all of them, choose under "Gilt für" further domains or "Alle Domains"
  (then the domains' own signatures are replaced by the one for every domain), and give single
  addresses their own under "Abweichende Signatur für einzelne Adressen".
- **Webmail**, Settings → Writing → Signatures: the same, with the rich editor.
- **API**: `GET`/`PUT /api/account/signatures` (portal) and `SignatureSettings/get`/`/set` over
  JMAP ([jmap-signatures.md](jmap-signatures.md)).

Each signature may take 256 KiB as text and as HTML (room for a small picture as a `data:` URL).
A change carries at most 500 signatures and is applied all or nothing; only the person's own
domains (the domains of their sending addresses) and own identities are accepted. Entries that
name the same domain (`*` and ` *`, `Example.ORG` and `example.org`) or the same identity count
once, the last one wins. An account has at most 2000 sending identities.

## Company signature (admin)

On the domain page, card "Firmen-Signatur", mode:

- **Aus**: nothing.
- **Vorlage** (template): people without a signature of their own for the domain get this one,
  filled with their name and address; the settings show it prefilled and they may change it.
- **Pflicht-Fußzeile** (mandatory footer): the server appends it on submission to every message
  whose `From` address is on the domain, from every door: JMAP `EmailSubmission` (webmail, apps)
  and SMTP submission (IMAP/SMTP mail programs). People's own signatures stay as they are.

Changes are recorded in the audit log as `domain.signature`
(`PUT /api/admin/domains/{name}/signature`, `{ "mode": "off" | "template" | "footer", "text",
"html" }`; the domain detail carries it as `signature`).

### How the footer goes in

- Before DKIM signing, so the signature covers it; the copy for a shared mailbox's Sent folder has
  it too.
- Placeholders are filled for the sender: `{name}` is the display name in `From`, else the
  account's name.
- With several addresses in `From`, the footer of the first one whose domain has a footer is
  used (a first address on a domain without one does not leave it out).
- Only the message's own text changes: a `text/plain` body gets the text footer below it, a
  `text/html` body gets the HTML footer before `</body>` (inside `<div class="uwumail-footer">`),
  a `multipart/alternative` gets both, and in `multipart/mixed`/`related` only the first part (the
  body before the attachments) is touched. Without an HTML footer the text is used, escaped; without
  a text footer the HTML is turned into text.
- A changed part is written anew as UTF-8: `7bit` when it is plain ASCII with short lines, else
  quoted-printable, whatever it was before (8bit, quoted-printable, base64, ISO-8859-x, Windows
  code pages). Text holding `--` anywhere (a `-- ` signature, say) is always written
  as quoted-printable, which writes a `-` at the start of any encoded line (also after a soft
  line break) and any `-` right after another as `=2D`, so the encoded text holds no `--` at all, and no text (nor a footer or a name with line breaks) can ever turn into a
  MIME boundary of the message. After the change the message must parse into the same structure
  with the content of every other part unchanged, else it is sent without the footer. A part in a charset the server cannot read back is left alone. Every other
  byte of the message stays as it was.
- The server's message size limit (`smtp.max_message_size`) holds for the message **with** the
  footer: a message that only fits without it is refused (`tooLarge` / `552 5.3.4`), for mail held
  back for undo send or send later already when it is submitted.
- **Signed or encrypted mail is left alone** (S/MIME `multipart/signed`, `application/pkcs7-mime`,
  PGP/MIME, inline PGP): a footer would break the signature or sit outside the encryption. The
  server logs `sent without the company footer` with the reason.
- **No double footer**: a part that already contains the footer (ignoring whitespace) gets none.
  Retries of the queue send the signed message as it is; mail held back for undo send or send
  later gets the footer once, when it goes.

Known limits:

- **The sender's copy in Sent has no footer.** The copy in Sent is the one the mail program saved
  (JMAP clients create the Email before they submit it, IMAP clients `APPEND` theirs); the server
  does not rewrite it, because a JMAP Email is immutable and replacing it would change its id
  under the client. Recipients get the footer, the sender's Sent folder shows the message as it was
  written. The portal's domain card says so too.
- A message without any text part (e.g. only an attachment) gets no footer.

### Best effort, not a compliance guarantee

The footer is a convenience for honest senders, not a control a sender cannot get around. A
message goes out without it when it is (or claims to be) signed or encrypted, when its body is an
attachment, when a text part uses a charset the server cannot write back, or when the text already
contains the footer anywhere, also hidden (e.g. in an HTML comment) or in a quoted earlier message.
Each skip is logged as `sent without the company footer` with the reason. Where a legal notice
must be on every message, do not rely on the footer alone.

### Known limitations (security review)

- The footer is put in on the async runtime, not in `spawn_blocking`; a large message ties up one
  runtime thread for the time it takes (bounded by `smtp.max_message_size`).
- `X-UwUMail-Label` headers a sender wrote are removed at submission (since 0.22.0), as inbound
  delivery already did, so no submitted message can bring its own label.

## Signature HTML is not sanitised by the server

The server stores and hands out signature HTML (the person's, per address and the company
template) as it was written: `Identity/get` `htmlSignature`, `SignatureSettings/get` and
`GET /api/account/signatures` return it verbatim, and any client or a direct JMAP call can store
arbitrary HTML. **Every client that renders it or puts it into a rich editor must sanitise it
first**, as the webmail does (`cleanSignatureHtml`) and the portal does by showing it only as text.
The company footer the server appends is the admin's HTML and goes into the outgoing message as it
is.
