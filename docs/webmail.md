# Web mail

The mailbox in a browser, served by this server under `/mail`. It exists for the
case where the [app](https://github.com/MinifyX/UwUMail-Client) isn't there: a
borrowed laptop, someone else's phone, a quick look from work.

The interface lives in its own repository,
[UwUMail-Webmail](https://github.com/MinifyX/UwUMail-Webmail). It is the app's
own interface, cut down to what a browser does well, and it talks to this server
over JMAP. This page is about the server's half.

## How it gets in

There is no second login. Whoever is signed in to the portal is signed in to the
webmail, because it is the same session cookie — so two-factor, passkeys, the
session list and locking someone out all keep holding, without any of it being
built twice.

JMAP therefore accepts two ways of signing in:

| | Who uses it | What it has to send |
| --- | --- | --- |
| `Authorization: Basic` | Mail apps, with the account password or an app password | Nothing else |
| The portal's session cookie | The webmail | The CSRF token in `X-CSRF-Token`, for anything but `GET` and `HEAD` |

Reading with the cookie alone is safe because the server sends no CORS headers
and the cookie is `SameSite=Strict`: another site can neither read an answer nor
get the cookie sent in the first place. Everything that changes something
carries the CSRF token, exactly like the portal's own JSON API.

Signing in this way is only for the webmail: it works while the webmail is
switched on, for accounts whose own switch is on, and never for a service
account. It deliberately does **not** look at the JMAP protocol switch — that
one decides what other mail programs may do with this account's password, and
taking someone's mail apps away should not quietly take away the browser too.

## Switches

| Where | What it does |
| --- | --- |
| Server → Settings → "Mailbox in the browser" (`http.webmail`) | The whole server. Off means `/mail` answers 404 and the session login for JMAP stops working. |
| People → a person → "Mailbox in the browser" | One account. Off means that person keeps every mail app and loses only the browser. |

Both default to on. As with every setting, the config file and `UWUMAIL_*`
environment variables win over the admin panel.

## What the server does that a browser may not

- **Cleaning message HTML.** `Email/get` offers `uwuSafeHtml` and
  `uwuHasRemoteContent` under the capability `urn:uwumail:jmap:webmail`. The
  rules are the app's (`crates/uwumail-jmap/src/safe_html.rs`): no scripts, no
  handlers, no frames, no forms, no `javascript:` — but tables, inline styles
  and the sender's layout survive, because that is what newsletters are made of.
  The webmail cleans a second time in the browser and shows the result in a
  frame without scripts, so a mistake in one place still has two doors to get
  past.
- **Building the message.** Sending goes through `Email/set` with a structured
  body, so the MIME is built here and not in a browser.
- **Fetching pictures.** A mail's remote pictures, once the reader lets them
  show, and the logos of company senders come through the server
  (`urn:uwumail:jmap:remote`, [jmap-remote.md](jmap-remote.md)). The sender
  sees the server — or the VPN in `[egress]` — and never who reads the mail.
  The page's policy allows pictures from its own origin only, so one that did
  not take that way can't load at all.

Two things the app does that the webmail leaves alone: it never asks the server
to follow a `List-Unsubscribe` link, because that would let a mail header decide
where this server sends requests. The webmail sends the unsubscribe mail itself
where the newsletter offers one, and otherwise opens the sender's page in a tab.
The one-click POST of RFC 8058 is missing as a result.

## How the webmail gets into the binary

`webmail.pin` names the repository and one full commit hash. The container build
clones exactly that commit, builds it with pnpm, and `crates/uwumail-web/build.rs`
embeds the result next to the portal. A branch name would mean the same server
tag builds something different tomorrow, so it is always a hash.

Moving to a newer webmail is a change to `webmail.pin` — visible in the diff,
visible in the changelog, and one CI run to prove it still builds.

For working on it locally, point the build at a folder instead:

```bash
UWUMAIL_WEBMAIL_DIST=../UwUMail-Webmail/dist cargo build -p uwumail-server
```

Without either, the server simply has no webmail: `/mail` is not routed, the
portal offers no button, and everything else works as before.

## What is not there yet

- Delayed sending, so "undo send" is real instead of a trick in one tab
- Signatures on the server, shared by the webmail, the app and Android
- Address suggestions from the address book and from mail history
- Web Push, so new mail arrives with the browser closed
