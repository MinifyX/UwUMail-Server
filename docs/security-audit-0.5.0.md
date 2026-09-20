# Security review — what 0.5.0 adds, and a webmail nobody had read yet, 20 September 2026

The fifth pass, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md),
[security-audit-0.3.0.md](security-audit-0.3.0.md) and
[security-audit-0.4.0.md](security-audit-0.4.0.md). Two things were looked at:

- **everything 0.5.0 adds to the server**: greylisted mail being kept and decided about in the
  portal, JMAP accepting the portal's session so a browser can read mail, the message HTML the
  server hands out for it, the webmail being served under `/mail`, the ports that can move, and
  the dormant data layer for fetching from foreign mailboxes that ships without a caller;
- **the whole of [UwUMail-Webmail](https://github.com/MinifyX/UwUMail-Webmail)**, a mail client
  that runs in a browser on the same origin as the portal and had never been read by anybody but
  the person who wrote it.

The threat model for the second part is the one that matters for mail: the attacker is somebody
who can send the reader an email. The body, the headers, the attachments and the sender's name are
all theirs to choose.

Done with Claude, not an independent firm. An honest sweep, not a certificate. Nothing was run
against the production server.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 0 | 0 | 0 |
| High | 1 | 1 | 0 |
| Medium | 3 | 3 | 0 |
| Low | 5 | 5 | 0 |

Nine findings. Eight are fixed in this release; the ninth is in code that ships dormant and was
fixed in the branch that will bring it to life. Six more things are deliberate and written down
under [Accepted](#accepted) so they are not rediscovered as findings later.

## Findings

### F-1 · High · A draft opened from the server put foreign markup into the app's own page

`UwUMail-Webmail`: `src/backend/jmap/JmapBackend.ts`, `src/features/compose/draft.ts`,
`src/features/compose/openDraft.ts`

The webmail shows a message in a frame with no scripts and a policy of its own. The composer is
not that frame: it is the app's own page, on the origin that holds the session cookie. Everything
that reaches it goes through `quotableHtml` first — every path but one.

Opening a draft did not. Three things had to line up, and they did:

- `openDraftThread` looked for a message with the draft flag and, finding none, fell back to the
  newest message of the conversation. A mail that merely sits in the Drafts folder could be opened
  as if it were a draft. It asked for one message, not the conversation, which made that fallback
  fire more easily than it looks.
- `openDraft` handed the body on as `bodyHtml ?? bodyText`. For a message with no HTML part —
  a plain `text/plain` mail — the server has nothing to clean and says so, and the raw text went
  on as if it were markup. Plain text that reads like a tag becomes one.
- The body then went into the editor with `innerHTML`, uncleaned.

So a plain-text mail whose body is `<img src=x onerror=…>`, moved into the Drafts folder or
answering into a conversation that holds a draft, ran its handler on the reader's origin.

In production the page's own policy caught the last step: `script-src 'self'` without
`'unsafe-inline'` blocks an inline handler. What it does not block is CSS — a `<style>` block from
a stranger could lay itself over the composer and its send button — or a remote image, which fires
a tracking pixel the moment the draft opens. The second line held, the first had a hole.

**Fixed** in `f0f7866` (webmail repo). Only a message that really carries the draft flag opens, and
the whole conversation is asked for so the draft is found even behind a newer message; a message
with no HTML part is escaped the way the reader escapes it; and a restored body goes through
`quotableHtml` like every other path. Nothing is lost by the last one: saving a draft already
cleaned it the same way, so a real draft comes back exactly as it was stored.

### F-2 · Medium · A decided message swallowed every later mail that shared its Message-ID

`crates/uwumail-store/src/greylist_hold.rs`

When greylisting keeps a message and its recipient decides about it, the row stays behind as a
tombstone: the sender's retry must not deliver the same mail twice or undo a discard. A returning
message was recognised by its bytes **or** by its `Message-ID` alone.

A `Message-ID` is a line the sender wrote. Anybody may put any of them on a mail, and plenty of
senders number theirs in a way that can be guessed — a repository's notifications, a ticket system,
a mailer that counts. So a tombstone was a two-day trap for every later message carrying that same
line, from any sender:

1. The attacker sends a message with the `Message-ID` the reader's next GitHub notification will
   carry, from an address that looks suspicious enough to be greylisted, and it is kept.
2. The reader sees the obvious spam in their waiting list and throws it away.
3. The real notification arrives inside the next two days. It matches the tombstone, the server
   answers `250`, and nothing is stored.

Neither side sees anything. The sender was told the mail was accepted, and the reader never learns
it existed. Delivering by hand instead of discarding sets the same trap.

**Fixed** in `46b5a1b`. A tombstone now only ever recognises the message it was made from, by its
bytes. The `Message-ID` still matches a row that is **still waiting**, where the worst it can do is
take an entry off somebody's list while the mail itself arrives normally. The cost is that a sender
who rewrites something between attempts can be delivered twice; dropped mail leaves nobody a trace,
so this side errs towards delivering twice. A test sends a different message carrying a decided
message's `Message-ID` and insists it arrives.

### F-3 · Medium · Unsubscribing by mail sent whatever the newsletter's header asked for

`UwUMail-Webmail`: `src/backend/jmap/JmapBackend.ts`, `src/features/mail/Unsubscribe.tsx`

`List-Unsubscribe` may name a `mailto:` address, and unsubscribing then sends a mail from the
reader's own account. Recipient, subject **and body** were taken from that header, and the dialog
asked "Unsubscribe from {name}?" and showed none of them — the name it shows comes from `From:`,
which has nothing to do with the address in `List-Unsubscribe`. The only check was that the address
contained an `@`.

So a mail with `List-Unsubscribe: <mailto:someone@elsewhere.example?subject=…&body=…>` turned one
click and one confirmation into a message with a stranger's words, sent under the reader's name, to
a recipient the reader never saw. Useful for confirming that an address is live, for answering a
"reply YES to confirm" flow, and for putting something compromising in somebody's sent folder.

This is not the one-click route of RFC 8058, which was deliberately left out (see
[Accepted](#accepted)): there the server would call on a foreign address. Here the reader's own
account sends the mail.

**Fixed** in `2e98b2c` (webmail repo). The body is not taken from the header at all. The subject
still is, because list managers match on it, but it stays on one line and within 200 characters.
The address has to be a single plain address — no commas, no line breaks, no angle brackets, and it
has to look like an address — and the dialog names it before anything is sent. Five tests cover the
smuggling attempts.

### F-4 · Medium · The webmail was built beside the credentials of the job that pushes the image

`.github/workflows/ci.yml`

The webmail comes from a second repository, pinned by full commit hash in `webmail.pin`, and
building it runs that repository's own package scripts. That happened inside the job that builds
and pushes the container image, which meant the foreign build ran next to two things it has no
business near: the credentials `actions/checkout` leaves in `.git/config` by default, in a job that
holds `packages: write`, and the server's own Rust sources, which are compiled a few steps later in
the same directory.

Whoever gets a commit into the webmail repository that is then pinned could have read the token and
pushed any image under the server's name, or changed the Rust sources before they were built, so
that the released image carried something the diff never showed. The pin does not help against
this: it decides *which* foreign commit gets those rights, not what it may do with them.

**Fixed** in `b27d89b`. The webmail builds in a job of its own with read-only rights and
`persist-credentials: false`, and hands the result over as an artifact, the way the portal already
does. The image job keeps no credentials either. The same isolation already existed in
`docker/Dockerfile`, whose webmail stage sees nothing but `webmail.pin`; only the CI path went
around it.

### F-5 · Low · The way in checked less than the way that shows it

`crates/uwumail-jmap/src/auth.rs`

Three conditions decide whether somebody may use the webmail: the server's switch, the account's
own, and whether the account has a mailbox at all — an account with neither IMAP nor JMAP hands its
mail to one address and stores nothing. The portal checks all three before it shows the way in.
`session_account`, which is where the cookie actually turns into JMAP access, checked the first
two.

So an account whose protocols an admin had switched off kept a working webmail: the button was
gone, but `/mail` answered, and reading old mail and sending both worked. An answer that only the
button knows about is not a rule.

**Fixed** in `2dcd1c4`. The enforcing path asks the same three questions as the one that shows the
way in. This does not bring back the JMAP protocol switch, which the webmail ignores on purpose:
an account with IMAP on and JMAP off still has a mailbox.

### F-6 · Low · The `.env` was written through a copy anyone on the machine could read

`install.sh`, `update.sh`

Every line the installer or the updater sets rewrites the whole `.env` through a temporary file
beside it. That file was created with whatever umask the shell had — 0644 on a stock Ubuntu — in a
directory created with 0755. For as long as it exists it holds everything the `.env` holds,
including the gateway pairing code, and a run that dies in between leaves it there for good.

This is the same class as F-3 of 0.4.0, which was fixed in `install.sh` for the file itself. The
copy it is written through was missed, in both scripts.

**Fixed** in `62538b9` and `082e9dc`. The copy is created with 0600, like the file it replaces, and
a trap removes it when a run ends early.

### F-7 · Low · A pairing code could write a second line into the `.env`

`install.sh`

Every answer the installer writes is checked character by character first — the host name, the
address, the language, the version, and now the ports. The gateway pairing code was checked only
for its prefix, and everything after `uwugw1` went into the `.env` as it stood. A line break in a
value passed with `--gateway-code` became a second, real assignment that Compose reads as its own
setting, for instance a different image tag than the one intended.

It needs a pairing code from a source the operator does not fully trust, and it gains nobody any
rights the script does not already have, which is why it is Low.

**Fixed** in `5a27e3e`. The code is held to letters, digits, dots, underscores and dashes.

### F-8 · Low · A click on an image map went around the link check

`UwUMail-Webmail`: `src/features/mail/MessageBody.tsx`

Links in a message are caught in the frame and handed to the app, which checks the scheme and warns
about a link whose text does not match where it goes. The handler looked for the closest `a[href]`,
and an `<area>` inside a `<map>` is neither an anchor nor inside one. A click on one would have
navigated the frame itself — the phishing page inside the familiar interface, no new tab, no
warning.

It does not bite today: the server's sanitizer keeps `href` on `<a>` and drops it everywhere else,
so an `<area>` arrives without one. That is a table of attributes in a library nobody here wrote,
and one version of it away from being different.

**Fixed** in `f4fe86c` (webmail repo). The handler catches `area[href]` as well.

### F-9 · Low · The dormant fetch store took a row id without an owner

`crates/uwumail-store/src/fetch.rs`

The data layer for fetching from foreign mailboxes ships in this release with no caller. Its
single-row functions took only the row id: `fetch_account(id)`, `update_fetch_account(id, …)`,
`delete_fetch_account(id)`, and `fetch_password(id)`, which answers with the clear-text password at
the foreign provider. Next door in `greylist_hold.rs` the same kind of function takes
`(account_id, id)` and every statement carries `AND account_id = ?`, with a test that says so.

The session that owns this code checked and corrected the finding: the portal route it is building
does compare the owner before every call, so the hole this describes would not have opened when the
route was switched on. What stands is the reason it was raised — a safeguard that depends on every
caller remembering it is not a safeguard, and the next endpoint is the one that forgets.

**Fixed** in the branch that will bring the feature to life, not in this release: the signatures
take `(account_id, id)`, every statement carries the pair, and a test walks all five functions with
a stranger's id. Nothing of it is reachable in 0.5.0.

## Also fixed while reviewing

Not security findings, but wrong, and found on the way:

- `install.sh` accepted any five-digit number as a port. `70000` has the right shape, went into the
  `.env`, and let the start fail at the end anyway — which is the one thing looking at the ports
  first exists to prevent. Fixed in `d5e7fbd`.
- `cargo fmt --check` had been failing on `main` since `fbf5429`, so three runs of CI stopped
  before the tests and nobody noticed. Fixed in `11a52a3`.

## What was checked and held

- **The portal session as a JMAP login.** The token is stored hashed, the cookie is `HttpOnly`,
  `SameSite=Strict` and `__Host-`-bound over HTTPS, the session exists only after a completed
  second factor, and the CSRF token is compared without letting the time taken say how much of it
  matched. Every request that changes something carries the CSRF header; the reading paths do not,
  which `SameSite=Strict` and the absence of any CORS header carry — there is no
  `Access-Control-*` anywhere in the server.
- **Serving attachments.** The account is compared, the blob is checked against that account, the
  response always carries `Content-Disposition: attachment` even when the file name has to be
  dropped, and `X-Content-Type-Options: nosniff` goes with it. The content type the caller may ask
  for therefore cannot bring anything to the screen on the server's own origin. Greylisted mail is
  not reachable this way: those blobs are in neither the mail nor the upload tables.
- **The sanitiser and the frame.** `<style>` survives on purpose, which is what newsletters are
  built from; the combination that makes that dangerous elsewhere is closed in this version of the
  library, and `script`, `iframe`, `form`, `base`, `svg` and `math` do not survive. The message is
  shown in a `srcdoc` frame without `allow-scripts`, carrying its own policy as a `<meta>` tag with
  `default-src 'none'` — so even something that got past both sanitisers would not run, and nothing
  loads from another server until the reader asks for it.
- **The waiting list.** Every query is scoped to the signed-in account, there is deliberately no
  admin route to any of it, and the list never carries a body, a link or an attachment. The blob
  reference counting in migration 0026 is symmetric across insert, delete and update, so no row can
  free a blob another one is still pointing at.
- **The cross-repository build itself.** Pinned by full commit hash, fetched at that hash and
  verified afterwards, so a renamed, transferred or forked second repository cannot change what a
  release contains without the check failing. Nothing is downloaded without a checksum, nothing
  over plain HTTP.
- **The container.** Distroless, pinned by digest, unprivileged user, read-only root, every
  capability dropped but the one that binds a port.
- **The portal front end.** No `dangerouslySetInnerHTML`, no `innerHTML`, nowhere. The CSRF token
  lives in a module variable and in no storage and no URL. The `?next=` after signing in was walked
  through the usual redirect tricks and holds.
- **SQL.** Every new statement binds its parameters. The only interpolation is a fixed list of
  column names.

## Accepted

Deliberate, and written down so they are not found again as if they were new:

- **One-click unsubscribe (RFC 8058) is not implemented.** It would have the server make a request
  to an address a mail header named. Unsubscribing opens the sender's page instead, or sends the
  mail the header asks for, with the limits under F-3. One click more, one surface less.
- **CSS is not filtered.** Neither in `style` attributes nor in `<style>` blocks. In the reader
  that is carried by a frame without scripts and with a policy of its own; the composer does not
  take CSS at all since F-1.
- **The "this mail loads nothing" hint can be wrong.** It reads the cleaned HTML for the shapes a
  remote reference takes; CSS escapes can hide one from it. A wrong answer there costs the banner,
  not the block: what may be loaded is decided by the frame's policy, not by this hint.
- **A setting changed on the command line needs a restart.** The command line writes to the
  database and has no channel to the running server, and it says so on every change. The portal
  applies every setting at once. It is worth knowing when a script sets `spam.greylist_hold` to
  false and moves on: messages keep being kept until the server is restarted.
- **The webmail is on after the update**, for the server and for every account, and can be switched
  off for either. It is a new, browser-facing surface that an update opens without asking.
- **Greylisted mail is kept for two days**, up to 5 MB per message and 200 messages per account,
  and a message that would be kept beyond that is greylisted the old way instead.

## What was not looked at

- The production server. Nothing was run against it, and nothing about it was changed.
- The portal and the webmail in a browser with a real login. Signing in means typing a password
  into a form, which is not something this review does; the interfaces were exercised against their
  mock, and the paths behind them against a real instance over the API.
- The dependencies themselves, beyond what the audit steps in CI check on every run.
- The gateway, which 0.5.0 does not change.

## What was actually run

For the record, because a report that says "checked" without saying how is worth less:

- The greylisting path end to end on the test instance, by the session that wrote it: a message
  greylisted and kept, the waiting list read over the API, all three decisions taken, and all three
  retries afterwards — delivered by hand, discarded, never decided. Delivered exactly once,
  discarded stayed discarded, undecided arrived normally.
- `cargo test -p uwumail-store greylist`, nine tests including the new one for F-2, and
  `cargo clippy --all-targets -- -D warnings` over the three crates this review changed.
- In the webmail repository: `tsc --noEmit`, `eslint`, `prettier --check` and 83 tests, five of them
  new for F-3.
- The webmail against a real server on the test instance, by the session that wrote it, with an
  image built the way a release is built — the clone from `webmail.pin` included. Signing in with
  the portal's session and then speaking JMAP, writing and sending a message and reading it back,
  and then the boundaries: a JMAP call without the CSRF token and with a wrong one (401 both), an
  upload without it (401), asking for another account's mailboxes (`accountNotFound`), downloading
  a blob under another account's id (404) and one that does not exist (404), and a download of
  one's own (`attachment`, `nosniff`). Both switches were turned off and on again: with the
  account's switch off JMAP answers 401 and the page says why, with the server's switch off `/mail`
  is gone — while the portal, Basic auth on JMAP and IMAP keep working, which is what the switch is
  for. That run also found a bug that no unit test could have: drafts were left behind on every
  save, because the server returns a Message-ID without the angle brackets the client wrote. Fixed
  and pinned.
- `bash -n` on both scripts, plus a run of the port check against valid ports, out-of-range
  numbers, empty input and two injection attempts, and a run of the `.env` writer against a sample
  file.
- The 0600 on the `.env` copy is Linux semantics and was not verified on this machine.
