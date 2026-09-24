# Branding and languages

UwUMail comes pink, with Nyu the envelope cat and a playful tone. All of that can be changed for
the whole server, for example when the server belongs to a club, a family business or a school.

## What can be changed

Under *Server → Settings → Branding* in the portal:

| | What it does |
|---|---|
| **Logo** | PNG, JPEG, WebP or SVG, at most 512 KB. Replaces Nyu in the sidebar, on the login page, in the webmail and as the icon in the browser tab. |
| **Name** | Shown instead of "UwUMail" in the browser tab, the sidebar, the webmail, and as the sender of mail the server writes itself (security notices, bounces to your own people, forwarding confirmations, the test mail). Mail apps see it too: autoconfig, Apple configuration profiles, the entry in authenticator apps and the name of passkeys. |
| **Accent colour** | One colour, picked from presets or freely. Every other shade for the light and the dark theme is worked out from it, and each is moved until its text reads at WCAG contrast 4.5:1 or better. A preview shows both themes before saving. |
| **Nyu and kaomoji** | Switched off, there is no cat anywhere, no faces like `(=^･ω･^=)`, and every text in the portal and the webmail, as well as every mail to your own people, uses the plain tone. The choice between playful and plain disappears for everyone. |

Leaving name and colour empty, removing the logo and keeping Nyu gives UwUMail exactly as it
comes. The colours of the default are never recalculated: without a chosen colour
`/branding.css` is empty and the built-in pink stays as designed.

The same settings exist in the config file and on the command line:

```toml
[brand]
name = "Post & Co"     # empty: UwUMail
color = "#0ea5e9"      # empty: the UwUMail pink
mascot = false         # Nyu and the kaomoji
```

```bash
docker compose exec uwumail uwumail-server settings set brand.name "Post & Co"
docker compose exec uwumail uwumail-server settings set brand.color "#0ea5e9"
docker compose exec uwumail uwumail-server settings set brand.mascot false
```

The logo is only set in the portal. It is kept in the database, so it is part of every backup.

### How it works

- `GET /branding.css` is a stylesheet with the accent tokens (`--uwu-pink`, `--uwu-pink-solid`,
  `--uwu-pink-ink`, `--uwu-pink-tint`, …) for `html:root` and `html:root[data-theme="dark"]`.
  The portal and the webmail both link it, so they always change together.
- `GET /branding/logo` hands out the logo. It is checked by its content when uploaded, and served
  with `Content-Security-Policy: sandbox` and `nosniff`, so an SVG cannot run scripts even when
  someone opens it on its own.
- `GET /api/info` (for the login page) and `GET /api/session` (for the portal and the webmail,
  under `server.brand`) carry `{ name, custom, color, mascot, logo }`.
- Changing the logo is written to the change log (*Server → Logs → Changes*); name, colour and
  mascot are ordinary settings and logged like them.

What stays UwUMail: the software's own name where it means the software — the UwUMail app, the
UwUMail Gateway, the SMTP and IMAP greetings, `Received:` headers — and the documentation.

## Languages

The portal, the webmail and the mail the server writes speak:

| Code | Language |
|---|---|
| `de` | Deutsch |
| `en` | English |
| `fr` | Français |
| `nl` | Nederlands |
| `ja` | 日本語 |
| `zh` | 简体中文 (Simplified Chinese) |

Everyone picks their own language under *Appearance and language* in the portal or in the
webmail's settings; "Same as the browser" takes the first of the browser's languages the server
speaks, and English when it speaks none of them. The choice is synced to the UwUMail apps as the
`language` user setting (see [jmap-settings.md](jmap-settings.md)).

Mail the server writes to someone follows that person's language. Bounces, and mail to someone
who left it to the browser, follow *Server → Settings → General → Mail from the server →
Language* (`tone.language`). New accounts get their first calendar and address book named in
that language too.
