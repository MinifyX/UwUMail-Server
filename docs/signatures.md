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
domains (the domains of their sending addresses) and own identities are accepted.

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
- Only the message's own text changes: a `text/plain` body gets the text footer below it, a
  `text/html` body gets the HTML footer before `</body>` (inside `<div class="uwumail-footer">`),
  a `multipart/alternative` gets both, and in `multipart/mixed`/`related` only the first part (the
  body before the attachments) is touched. Without an HTML footer the text is used, escaped; without
  a text footer the HTML is turned into text.
- A changed part is written anew as UTF-8: `7bit` when it is plain ASCII with short lines, else
  quoted-printable, whatever it was before (8bit, quoted-printable, base64, ISO-8859-x, Windows
  code pages). A part in a charset the server cannot read back is left alone. Every other byte of
  the message stays as it was.
- **Signed or encrypted mail is left alone** (S/MIME `multipart/signed`, `application/pkcs7-mime`,
  PGP/MIME, inline PGP): a footer would break the signature or sit outside the encryption. The
  server logs `sent without the company footer` with the reason.
- **No double footer**: a part that already contains the footer (ignoring whitespace) gets none.
  Retries of the queue send the signed message as it is; mail held back for undo send or send
  later gets the footer once, when it goes.

Known limits: the sender's own copy in Sent is the one the mail program saved, without the footer
(JMAP clients and IMAP clients store their own copy). A message without any text part (e.g. only an
attachment) gets no footer.
