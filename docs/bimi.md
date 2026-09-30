# BIMI

With BIMI (Brand Indicators for Message Identification) a domain publishes its
logo, and mail apps that support it show that logo next to mail from the
domain. The server hosts the logo and tells the admin which record to publish.

**Who shows it:** Gmail, Apple Mail (iCloud), Yahoo and AOL, Fastmail and some
others. **Microsoft Outlook does not**, and Microsoft has not said when it will.
Gmail and Apple only show a logo with a mark certificate: a Verified Mark
Certificate (VMC, needs a registered trademark) or a Common Mark Certificate
(CMC, a logo used for a year or more), bought from a certificate authority.
Yahoo shows logos without one when the domain sends enough mail with good
reputation.

## Setting it up

On the domain's page, in the *BIMI* section:

1. **Upload the logo as SVG.** The domain's logo from the profile pictures is a
   pixel image (PNG or JPEG) and cannot become a BIMI logo: BIMI needs a vector
   drawing, and tracing pixels into one does not give a logo anyone would want
   to show. The server turns the SVG into SVG Tiny PS, the profile BIMI
   requires:
   - `version="1.2"`, `baseProfile="tiny-ps"`, a `<title>` (the name given, the
     one in the file, or the domain's name) and a square `viewBox`, centred on
     the drawing;
   - optionally a solid square behind the logo in a chosen colour (recommended:
     mail apps show the logo in a circle or a square);
   - style sheets and `style` attributes become attributes (simple class, id and
     element selectors);
   - scripts, event handlers, animation, metadata and editor extras are left
     out; links (`<a>`) keep only what they contain;
   - refused, by name, instead of silently changing the look: embedded pixel
     images, references outside the file, clip paths, masks, filters, patterns
     and anything else SVG Tiny 1.2 cannot draw. The admin converts those in
     the design tool (e.g. "flatten", "expand appearance", "convert text to
     paths") and uploads again;
   - at most 32 KB afterwards.
2. **Optionally add the mark certificate** (PEM, with its chain). It is checked
   to be a mark certificate (extended key usage
   `1.3.6.1.5.5.7.3.31`), and the portal shows its kind, names and dates.
3. **Switch BIMI on.** The logo is served at
   `https://<host name>/bimi/<domain>.svg` and the certificate at
   `https://<host name>/bimi/<domain>.pem`, for everyone, only while BIMI is
   on (as `image/svg+xml` with a sandboxing content security policy).
4. **Publish the record** `default._bimi.<domain>`:
   `v=BIMI1; l=https://<host name>/bimi/<domain>.svg; a=https://<host name>/bimi/<domain>.pem`
   (`a=` only with a certificate). While BIMI is on, the DNS check looks for it
   (optional, like the SRV records), and the Cloudflare button puts it in.
5. **DMARC has to be enforced:** `p=quarantine` or `p=reject`, for all mail
   (`pct=100`, which is the default), and not `sp=none`. The section says what
   is missing.

## API

| | |
| --- | --- |
| `GET /api/admin/domains/{name}/bimi` | The setup, the record, what the DNS check saw, DMARC and the certificate |
| `PUT /api/admin/domains/{name}/bimi` | `{ enabled?, title? }` |
| `PUT /api/admin/domains/{name}/bimi/svg` | `{ svg, title?, background? }`, cleaned into SVG Tiny PS |
| `DELETE /api/admin/domains/{name}/bimi/svg` | Removes the logo and switches BIMI off |
| `GET /api/admin/domains/{name}/bimi/logo.svg` | The logo for the preview, also while off |
| `PUT`, `DELETE /api/admin/domains/{name}/bimi/certificate` | `{ pem }` |
| `POST /api/admin/domains/{name}/bimi/check` | Checks the domain's DNS now |
| `GET /bimi/<domain>.svg`, `GET /bimi/<domain>.pem` | Public, while BIMI is on |
