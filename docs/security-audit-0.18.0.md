# Security audit — what 0.18.0 added

A pass over what is new in 0.18.0, after [security-audit-0.17.0.md](security-audit-0.17.0.md):
fetched mailboxes signing in at Microsoft and Google (OAuth 2, SASL XOAUTH2), the picture cache and
the sizes of message pictures, the egress proxy's rests, reading the text in pictures (OCR), the
birthdays calendar with `Birthdays/scan` and `Birthdays/import`, and the AI assistant (its crate,
the JMAP extension with its event stream, the portal pages, the ChatGPT sign-in and the label queue).

Scope: `git diff` of `main` (0.17.1) against `release/0.18.0` — `uwumail-smtp` (provider sign-in,
egress, picture cache, autoconfig, XOAUTH2 in the SMTP client), `uwumail-server` (fetching with a
grant), `uwumail-store` (fetch grants, birthdays, assist), `uwumail-jmap` (pictures, OCR,
birthdays, calendar events, assist methods and stream), `uwumail-assist`, `uwumail-web` (fetch
sign-in and assist routes) and `web/` (sign-in panel, assist pages).

Done with Claude, not an independent firm: the four areas were read in parallel, each finding traced
through the code and, from Medium up, given a regression test that fails before the fix. Not a
certificate. No exploit code or attack payloads are written down here; the analysis says what was
wrong and the diffs show how it was fixed. Nothing was run against a production server.

The rule is the same as before: everything **Medium and above is fixed**; Low findings are fixed
where the fix is small and safe, the rest listed.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 0 | — | 0 |
| High | 1 | 1 | 0 |
| Medium | 8 | 8 | 0 |
| Low | 21 | 12 | 9 |
| Informational | 7 | 2 | 5 |

No panic on text from outside this time: the new readers (vCard dates and `X-` parameters, the
birthday titles, image tags in HTML, SVG sizes, the event stream of providers, link collection,
subjects of drafts, the OCR text) cut strings only at boundaries they found themselves, by
characters, or through `get()`. The serious findings are about **cost**: one account could make
everyone's calendar requests hold the database writer (BDAY-1), and several new paths did expensive
work or talked to someone else before a limit was looked at. And one about **where a token goes**:
the servers of a mailbox signed in at its provider could still be changed, so its access token went
wherever the person pointed it (OAUTH-01).

Nothing found lets someone into another account's mail. The new cross-account paths (the picture
cache, the label queue, birthdays of address books shared with the account) are scoped by account
or hold nothing personal.

## High

| ID | Severity | What was wrong | Fix |
| --- | --- | --- | --- |
| BDAY-1 | High | `crates/uwumail-store/src/birthdays.rs` (`rebuild`, `follow_language`). When a person's language changed, the next calendar listing (JMAP `Calendar/get`, every CalDAV PROPFIND) rewrote the birthdays calendar inside its write transaction. With the calendar full (50 000 entries, reachable with a few thousand dated cards, also by someone the address book is shared with for writing) the first new entry failed with `QuotaExceeded`, the whole transaction and the new language were rolled back, and every following listing started again: calendars broken for that account, and each client poll held the one database writer for seconds, which held up delivery for everyone. The loop over the cards was quadratic as well. | A full calendar leaves dates out during a rebuild, as it already did for a single card; entries of cards that are gone are removed first; the cards are looked up in a set. |

## Medium

| ID | Location | What was wrong | Fix |
| --- | --- | --- | --- |
| OAUTH-01 | `uwumail-store/src/fetch.rs` `update_fetch_account`, `uwumail-web/src/routes/fetch.rs` | A mailbox signed in at Microsoft or Google still took new IMAP and SMTP servers and a new user name. Fetching and answering then sent the live access token (for Google the whole mailbox) to that host; a stolen portal session could keep collecting fresh tokens. | The servers and the user name of a signed-in mailbox change only together with a new grant or a password; sending the same values again (the edit form does) stays allowed. The create route passes the grant along with the proven outgoing server. |
| BDAY-2 | `uwumail-store/src/birthday_import.rs` `scan_birthday_events`, `writable_cards`, `known_card` | `Birthdays/scan` collected up to 20 000 whole events of up to 1 MiB each in memory, and read every writable card with any number of nicknames: anyone who can fill a calendar or book the person sees could make it take gigabytes. | Entries are read one at a time; events and cards over 64 KiB are not scanned; at most 20 000 cards and 12 names per card. |
| EGRESS-1 | `uwumail-smtp/src/egress.rs` `Breaker`, `Connector::open` | Eight refused tunnels to three hosts made the proxy rest for the whole server. Anyone could ask for pictures on dead hosts and so send everyone's pictures, fetches and unsubscribes out directly from the server's address (`fallback = direct`) or nowhere (`block`), and keep it that way: after the rest any failing request, even one started before, ended the trial. | Before refused tunnels rest the proxy, the last address it reached is asked again through it; only when that fails too does it rest. After a rest only the request that tries the proxy again decides. |
| AI-01 | `uwumail-assist/src/access.rs`, `uwumail-store/src/assist.rs`, `uwumail-jmap/src/assist_stream.rs` | Usage was counted after the answer. A stream whose client left (or a dropped JMAP call) was never counted; failures counted no tokens; and check and count were separate, so requests side by side shared the last one left. | `reserve_assist_usage` checks the day's limits and counts the request in one write before the provider is asked; tokens are settled afterwards, estimated from what was sent and received for failures, and by a drop guard for a request whose listener went away. |
| AI-02 | `uwumail-assist/src/features.rs` | Summaries of a thread (up to 20 messages of up to 50 MB parsed), picture text (OCR) and the other features read the mail before the policy, the quota and the per-person concurrency were looked at, so a person over quota or without a provider could still start any number of expensive requests. | `prepare` (policy, provider, running slot, quota) comes before anything is read; at most 4 MB of a message are parsed for a prompt. Also closes OCR-4. |
| AI-03 | `uwumail-assist/src/llm.rs` `SseParser::feed` | The parser of a provider's event stream looked at the whole buffer (up to 1 MB) again with every chunk and shifted it for every line: a provider a person typed in could keep a core busy for minutes per request. | Each byte is looked at once, the buffer shifted once per chunk; a line has at most 256 KiB. |
| AI-04 | `uwumail-assist/src/worker.rs`, `uwumail-store/src/assist.rs` | The label worker took jobs one after another, each for up to three minutes, with retries; one person's hanging provider held up everyone's labels, and a person's queue had no bound (every delivered mail, from any sender). | One job per person per batch, four side by side, 45 seconds each (then tried again later); at most 200 mails waiting per person. |
| OAUTH-02 | `uwumail-web/src/routes/fetch.rs` `start_sign_in`, `detect`; `uwumail-smtp/src/provider_oauth.rs` | Starting a sign-in asked Microsoft for a device code under the client id every installation shares, without a limit (the per-person cap of three flows evicts the oldest instead of refusing); the provider look did MX lookups for any domain without one. Microsoft throttling the shared id would stop sign-ins everywhere. Rated Low by the reviewer, raised because the id is shared. | 20 starts and 300 looks per person an hour; a full flow table answers before Microsoft is asked. |

## Low

- **OAUTH-03** — the IMAP and SMTP proof of a sign-in runs in the portal's poll request; a browser
  that leaves then holds one of the person's three flows for ten minutes, and a store error after the
  proof throws a valid sign-in away. **Open** (costs the person only).
- **OAUTH-04** — a token renewal under way can overwrite a new sign-in's tokens or mark it expired.
  **Open;** a generation column on the grant would fence it.
- **OAUTH-05** — `set_grant` updated by row id alone, against the file's own rule. **Fixed:** it asks
  for the owner too.
- **BDAY-3** — `uwuBirthday` was read from any event carrying the marker, an invitation from outside
  included, with its year taken unchecked (the age arithmetic could overflow). **Fixed:** only the
  birthdays calendar's entries are decorated, years 1–9999, checked subtraction.
- **BDAY-4** — importing into a card of a book shared with the person deletes their event, but the
  date lands in the owner's birthdays calendar. **Open** (a product decision; the scan says which
  book a card is in).
- **BDAY-5** — every card write counts the entries of the birthdays calendar. **Open** (cost only).
- **IMG-1** — a person asking for a picture another person's request is already fetching waits in
  that person's queue. **Open.**
- **IMG-2** — cache hits read the whole file (up to 10 MB) into memory without a bound on how many
  at once. **Open.**
- **IMG-3** — half-written cache files were named after the process id, always 1 in the container,
  so leftovers of a crash were never removed or counted. **Fixed:** random names, leftovers older
  than ten minutes go at the next start.
- **OCR-1** — the deadline of `Email/imageText` did not cover fetching remote pictures. **Fixed.**
- **OCR-2** — two OCR runs at a time for the whole server, shared by everyone, no dedup of the same
  picture. **Open.**
- **OCR-3** — shrinking a 40-megapixel picture built a floating-point copy of about 250 MB besides
  the decoded one. **Fixed:** grey first, then shrunk by whole pixels.
- **EGRESS-3** (from before 0.18) — NAT64, 6to4, IPv4-compatible and Teredo addresses counted as
  public whatever IPv4 address they carry. **Fixed.**
- **AI-05** — requests side by side each renewed a ChatGPT sign-in with the same refresh token,
  which can end the sign-in. **Fixed:** one renewal at a time, the secret read again first.
- **AI-06** — polling the ChatGPT sign-in asked OpenAI on every poll. **Fixed:** not again within
  OpenAI's interval. Model lists (`AssistProvider/models`) are still asked for each time.
- **AI-07** — the last four characters of the admin's key reached people through `keyHint`.
  **Fixed:** only admins see it.
- **AI-08** — people can choose any model name on a server provider, so the admin's key pays for
  the most expensive one. **Open;** the token quota bounds it.
- **AI-09** — service accounts and shared mailboxes with app passwords get the assistant (their own
  quota, providers and labels). **Open,** to decide.
- **AI-10** — subject and display names were not escaped in prompts, so they could close the mail's
  data block. **Fixed.**
- **OAUTH-06** — user names of fetched mailboxes could hold control characters (extra SASL fields
  in the person's own XOAUTH2 string, CR/LF in `LOGIN`). **Fixed:** refused.
- **BDAY-7** — a `deleteEvent` that was not a boolean counted as true. **Fixed:** refused.

## Informational

- OAUTH-07 — any person on any installation can make device codes under the shared Microsoft
  client id (RFC 8628 §5.4 device-code phishing); an admin can set a client id of their own.
- IMG-4 — the picture cache is keyed by the address alone, so anyone who knows an address can learn
  whether someone here loaded it in the past week (the sender learns that anyway).
- OCR-4 — OCR ran before the assistant's quota was looked at; fixed with AI-02.
- EGRESS-2 — `allow_personal_private` opens all of RFC 1918, CGNAT and IPv6 ULA to people's own
  providers, in Docker also other containers and the host; admin only and re-checked at every
  connection. An allowlist of ranges would be narrower.
- BDAY-6 — someone who may write an address book shared with a person can give its cards reminders
  that ring its owner (bounded by the reminder limits of 0.17.0).
- AI-11 — `Assist/extractEvents` looks up each participant address one by one.
- AI-12 — types holding secrets derived `Debug`, and the label worker logged the provider's error
  text. Fixed: `Debug` leaves secrets out, the worker logs the kind of failure only.

Two notes that are no findings:

- `egress.assist` does not open private addresses: it only decides whether the assistant's requests
  take the proxy. No other egress user (pictures, avatars, unsubscribe, push, OAuth, fetch) reaches
  anything but public addresses.
- The picture cache is shared by design: a fetch carries no cookie, login or referrer, so what one
  person caches is what anyone would have got.

## What held up

- **Google callback:** `state` is 24 random bytes, single use and ten minutes; PKCE S256; a
  `__Host-` cookie binds it to the browser (HttpOnly, Secure, SameSite=Lax, compared in constant
  time), a wrong cookie does not use the state up. Tokens land only in the flow's owner; `poll`,
  `settle` and `take` check the owner, `take` works once. `redirect_uri` comes from the configured
  hostname, not the `Host` header; the callback only redirects to the fixed fetch page.
- **Grants:** refresh and access tokens sealed with the existing `seal`; `Debug` leaves them out; no
  API answer carries them; errors say `AUTHENTICATE` without the command. The Microsoft tenant is a
  fixed word, never user input; token endpoints are fixed hosts reached through the egress with
  timeouts and a 64 KiB answer. Device-code polling happens only when the portal asks and keeps
  Microsoft's interval; at most 256 flows, three per person.
- **Pictures:** cache files are named by SHA-256, no path from the address; type by magic bytes,
  SVG served with `nosniff`, a sandbox CSP and as an attachment; 10 MB per picture, the cache within
  `image_cache_mb`; the sizes endpoint wants the CSRF token, takes 200 addresses and 1 MB of body;
  every connection resolves and checks the address again, through the proxy too, each redirect
  checked.
- **OCR:** no argument from the person reaches tesseract (languages from the config, the picture on
  stdin); killed on timeout; output capped by characters; decoding with limits and a pixel check
  first; tesseract only sees a grey PNG the server wrote; remote pictures only with `remote: true`.
- **Birthdays calendar:** read-only over CalDAV (PUT, DELETE, collection DELETE refused; MOVE, COPY
  and ACL not offered) and JMAP (`CalendarEvent/set` and `/copy` into it, `Calendar/set` destroy);
  sharing it gives no write rights. Scan and import refuse shared accounts, read only visible
  events, write only writable books, check ETags in one transaction, at most 500 entries per import.
  The dates of a shared book go only into its owner's calendar. Dates are clamped to 1900–2100, 29
  February handled, titles cut by characters.
- **Assistant:** its methods are not available in shared accounts; every mail is read with the
  caller's account id; people see only providers they are allowed; keys sealed and never returned;
  providers people type in reach public addresses over https only unless the admin allows the local
  network; no redirects; answers bounded (1 MB, 60 s idle, 180 s in all); auto-labels are exact
  matches from the person's own labels, only add keywords, logged and undoable; the spam check only
  answers; events must quote the mail. The ChatGPT sign-in is a device flow whose tokens stay sealed
  on the server.
