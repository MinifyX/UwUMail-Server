# Security audit — 0.38.0, carrier lookup, parcel cross-check, shop tracking links and the Spam-Netz

Scope: the new code since 0.37.x on `fix/shipment-detection`. That is `crates/uwumail-smtp/src/parcels.rs` (carrier lookup, budgets, `Shipment/refresh`, the cross-check of doubtful parcel mail), `crates/uwumail-smtp/src/spam/spamnet.rs` with `crates/uwumail-store/src/spamnet.rs` and migration `0098_spamnet.sql`, `crates/uwumail-smtp/src/spam/parcels.rs` (the new parcel spam rules), `shop_link` in `crates/uwumail-labels/src/shipment.rs` with `shopTrackingUrl` in `crates/uwumail-store/src/shipments.rs` (migrations 0096, 0097), the wiring in `inbound.rs`, `spam.rs`, `spam/content.rs`, `egress.rs` and `mutate.rs`, `Shipment/refresh` in `crates/uwumail-jmap/src/methods/shipment.rs`, and the admin routes and settings in `uwumail-web` and `uwumail-server`.

Done with Claude, not an independent firm; it reads the code and does not replace a penetration test. Nothing was built or run for it.

## Summary

| Severity | New | Fixed | Accepted / open |
|---|---|---|---|
| Medium | 1 | 1 | 0 |
| Low | 5 | 5 | 0 |
| Info | 4 | 2 | 2 documented (S-7, S-8) |

All findings were handled in c06a4ef8 (and ae3718dd for S-5): S-1 to S-6, S-9 and S-10 fixed, S-7 and S-8 documented in `docs/spam-filter.md`.

Nothing blocked the release as long as the Spam-Netz stays off by default (`spamnet.fetch`/`spamnet.report` are `false`). S-1 should be fixed before an admin is told to switch fetching on. S-2 to S-4 make the parcel cross-check move or learn from mail more readily than intended. They are worth fixing in this release because the cross-check runs as soon as the carrier lookup is on.

## Threat model

- **A stranger sending mail.** They control everything in an unauthenticated mail: the From header, subject, text, numbers and links. They can send many such mails. They may want to get a phishing mail past the filter, push legitimate mail into Junk, use up the server's carrier budgets, or damage the reputation of a domain they do not own.
- **A shop with its own authenticated domain.** It controls the links in its own mail, but not other shops' domains.
- **A signed-in person.** They may add shipments with any number, ask for refreshes, and mark mail as spam. They must not reach other accounts' shipments or exhaust what all accounts share.
- **The Spam-Netz hub.** It is trusted to rate mail, but may be wrong, compromised, or fed by other participants who lie. What it sends must stay bounded, and what the server sends it must stay what the docs promise.
- **A carrier API.** Its answers are untrusted input with limited size and time.

## Findings

### S-1 — Spam-Netz `from` reports come from forged senders and land on authenticated mail (medium, fixed)

- **Place:** `crates/uwumail-smtp/src/spam/spamnet.rs:153` (`add_findings`), `:191` (`report_refused`), `crates/uwumail-store/src/spamnet.rs:54` (`queue_marked`), `crates/uwumail-smtp/src/spam.rs` (the call site).
- **Cause:** Unauthenticated mail is reported along with the registrable domain of its From header. A spammer who forges a legitimate domain in From therefore builds up a bad `from` score for that domain on the hub. The list is then applied to every incoming mail, including DMARC-authenticated mail, the person's contacts and allowlisted senders, for up to 6 points (4 per kind). The same applies to `domain` and `ip` entries for big shared providers, and to a hub or participant that lies. The mail of legitimate domains can then land in Junk on every participating server.
- **Fix:**
  - Report `from` only when the mail was DMARC-aligned for that domain, and leave it out otherwise.
  - Do not apply `from` entries to mail that is authenticated for that domain.
  - Do not apply any list entry to mail from contacts or allowlisted senders.
  - Consider keeping the total below the junk threshold, so the network can only push mail that is already doubtful over the line.
- Fixed in c06a4ef8: `Features` sends `from` only when DMARC passed for it; an unaligned From domain is still looked up, an aligned one is not. Mail whose From is verified and that a recipient knows (contact or correspondent) loses all `SPAMNET_*` points; an allowed sender stays out of Junk anyway.

### S-2 — A sender or a person can use up the carrier budget for everyone (low, fixed)

- **Place:** `crates/uwumail-smtp/src/parcels.rs:237` (`take`), `:502` (`cross_check`), `crates/uwumail-store/src/shipments.rs` (`claim_shipment_refresh`, `SHIPMENT_REFRESH_SECS`), `crates/uwumail-jmap/src/methods/shipment.rs` (`refresh`, `MAX_REFRESH`).
- **Cause:** Foreground lookups may take the full daily cap, and they share the `used` counter with the background worker. Two things make foreground lookups:
  - The cross-check, for every unauthenticated mail that looks like a carrier brand, with up to 3 numbers per mail. A stranger can send many such mails.
  - `Shipment/refresh`. Its limit is once in 5 minutes per shipment, not per account, and a person can add any number of shipments with numbers through `Shipment/set`, then refresh 20 per call.

  Either way, the day's cap (DHL's is small) is gone. After that, background polling and refreshes fail for every account, and the cross-check itself fails open.
- **Fix:**
  - Give the cross-check its own small daily share, and cap it per sending IP or domain.
  - Cap `Shipment/refresh` per account, for example 20 parcels per 5 minutes, and keep a reserve of the cap for the background worker.
- Fixed in c06a4ef8: people get at most 40 % and the cross-check 10 % of a carrier's day, the worker keeps 80 % for itself; the cross-check counts at most 5 mails per sender (DMARC domain, else /24 or /64) a day, and `Shipment/refresh` at most 20 parcels per account in 5 minutes (`tooSoon` after that).

### S-3 — The cross-check's automatic Junk move counts as the person's own mark (low, fixed)

- **Place:** `crates/uwumail-smtp/src/parcels.rs:544` (`update_emails_by_server`), `crates/uwumail-store/src/mutate.rs:230` (`junk_signal`, which `by_hand` does not gate).
- **Cause:** When the carrier does not know any of the numbers, the mail is moved to Junk through the normal update path. `junk_signal` runs there whatever the source. So the server's own guess:
  - trains Bayes,
  - rebooks the sender's verdict,
  - queues a Spam-Netz report with reason `user`.

  A wrong guess (S-4) then feeds back into the filter and into the hub as if a person had confirmed it.
- **Fix:** Pass the source into `update_one` and skip the learning, the rebooking and the Spam-Netz queue for moves the server made. Keep `forget_junk_mail`, so the parcel still goes.
- Fixed in c06a4ef8: `Batch::by_server` (set by `update_emails_by_server`) skips the sender verdict, Bayes and the Spam-Netz report; the parcels of the mail are still forgotten.

### S-4 — Forwarded or mailing-list mail about a fresh parcel can go to Junk (low, fixed)

- **Place:** `crates/uwumail-smtp/src/parcels.rs:502` (`cross_check`), `send()` (404 is mapped to `notFound`).
- **Cause:**
  - Mail that broke DMARC on the way, such as forwarded mail or a mailing list, counts as unauthenticated. If its subject also looks like a carrier brand, it is doubtful.
  - The official APIs often answer 404 for a label printed minutes ago that has not been scanned yet. A 404 from a proxy or a load balancer counts as `notFound` as well.

  Together, a real shipping notice can be moved to Junk.
- **Fix:**
  - Treat only the carrier API's explicit not-found body as unknown, not a bare 404.
  - Move only when the mail was also `Suspicious` on its own, or ask again after a delay (for example 6 hours) before moving.
- Fixed in c06a4ef8: only mail the filter calls Suspicious is cross-checked (the brand rules no longer start it), and a 404 counts as not found only when its body is the carrier's JSON; any other 404 is `unavailable`.

### S-5 — The cross-check ignores an Allow decision (low, fixed)

- **Place:** `crates/uwumail-smtp/src/inbound.rs:2284` (the `ParcelCheck` push).
- **Cause:** A check is pushed for every unauthenticated mail that has a carrier and numbers, whatever the decision. `Decision::Allow` only changes whether the parcel is recorded. Mail from a sender the person allowlisted can therefore still be moved to Junk.
- **Fix:** Do not push a `ParcelCheck` when the decision is `Allow`, or when the sender is an authenticated contact.
- Fixed in c06a4ef8 and ae3718dd: no `ParcelCheck` when the decision is `Allow`, nor when the From address is verified (SPF or DKIM for its domain, or DMARC pass) and the recipient knows it (contact or correspondent).

### S-6 — The Spam-Netz list has no entry limit and is applied in one write (low, fixed)

- **Place:** `crates/uwumail-smtp/src/spam/spamnet.rs:359` (`fetch_list`, `MAX_ANSWER` 16 MiB at `:28`), `apply_spamnet_list` in `crates/uwumail-store/src/spamnet.rs`.
- **Cause:** Kinds and value lengths are checked, but the number of entries is not. Up to 16 MiB of entries (hundreds of thousands) are parsed and written in a single store transaction. That holds the one writer, and so delivery, for the whole time. Each page also follows the cursor, up to `BATCHES_PER_RUN` times.
- **Fix:** Cap the entries per page (for example 50 000) and per run, and apply them in chunks of a few thousand per transaction.
- Fixed in c06a4ef8: at most 50 000 entries (and removals) per fetch, written in chunks of 2 000 per transaction; a fetch that fails while applying drops the cursor so the next one gets the full list. Correction to the cause: `fetch_list` fetches one page per run; `BATCHES_PER_RUN` limits reports only.

### S-7 — Registration sends the host name and the version (info, documented)

- **Place:** `crates/uwumail-smtp/src/spam/spamnet.rs:278`.
- **Cause:** Registration posts `name: <hostname>` and `software: "UwUMail <version>"`. The module doc and `docs/spam-filter.md` say that only four kinds of value ever leave the server. The host name identifies the server and the domain, and the version tells the hub which fixes it lacks.
- **Fix:** Name both in the docs and the admin text, or send a name the admin chooses (empty by default).
- Documented: `docs/spam-filter.md` (Spam-Netz) and the module doc name the host name and `UwUMail <version>` sent at registration. An admin-chosen name would need config, settings, the portal card and the mock API, so it stays open for later.

### S-8 — Digests can confirm a known text, and ham reports cover personal mail (info, documented)

- **Place:** `crates/uwumail-store/src/spamnet.rs:54` (`queue_marked`), digest v1 in `crates/uwumail-smtp/src/spam/spamnet.rs`.
- **Cause:** A digest is an unsalted, truncated SHA-256 of the normalized text. When a person takes mail from a non-contact out of Junk, its digest goes to the hub as ham. That mail may be personal, such as a first mail from someone new. Anyone who holds the same text can confirm that this server received it.
- **Fix:** Document this. Send ham reports only for mail the filter itself had put in Junk, which is already the case, and leave out mail with few recipients or a reply from the person.
- Documented: `docs/spam-filter.md` says that the digests are unsalted and can confirm a known text, and that a not-spam report can come from personal mail the filter had put in Junk (ham is reported only for mail moved out of Junk).

### S-9 — The parcel rules hit common words and authenticated mail (info, fixed)

- **Place:** `crates/uwumail-smtp/src/spam/parcels.rs:25` (`CARRIER_NAMES` includes `"post"`), `:68` (`PARCEL_BAD_NUMBER`), `:77` (`PARCEL_FEE_UNAUTH`), `:99` (`PARCEL_FOREIGN_LINK`).
- **Cause:** The rules have no panics; all slicing is on ASCII. But:
  - `PARCEL_BAD_NUMBER` (2.5) also fires for authenticated mail, and without any parcel words.
  - `PARCEL_FEE_UNAUTH` (3.0) matches words such as "lieferung", "delivery", "tracking", "nachzahlung" and "gebühr von". Those also appear in bills from domains without DMARC and in forwarded mail.
  - `PARCEL_FOREIGN_LINK` (2.0) counts "post" and "tracking", which are common in newsletters with ESP click domains.
- **Fix:**
  - Gate `PARCEL_BAD_NUMBER` on unauthenticated mail that also has a parcel word.
  - Drop "post" from `CARRIER_NAMES`, or match "deutsche post" only.
  - Require a carrier name and a number together before the fee and link rules fire.
- Fixed in c06a4ef8: all parcel rules need unauthenticated mail with a parcel word, `PARCEL_BAD_NUMBER` included; the fee and link rules also need a carrier name and a parcel-like number (8–40 letters and digits, 8 of them digits); "post" alone is no carrier name any more.

### S-10 — `shop_link` keeps other trackers and does not check the host's characters (info, fixed)

- **Place:** `crates/uwumail-labels/src/shipment.rs:895` (`TRACKER_PREFIXES`), `:898` (`shop_link`).
- **Cause:** The link is safe: it is https only and rebuilt as `https://{host}{path}`, so no `javascript:` or `data:` link can come out of it. Its host must equal the authenticated From domain or a subdomain of it, it is set only from authenticated mail, and it is never fetched. Two smaller points remain:
  - Only the `utm_`, `oxm_` and `mc_` prefixes are dropped. `fbclid`, `gclid`, `_hsenc`, `mkt_tok` and similar trackers stay, and so does the fragment.
  - The host is not limited to `[a-z0-9.-]` as `Carrier::of_host` has been since 0.36.0. Odd characters only reach the shop's own domain, so the risk is spoofing in the display rather than leaving that domain.
- **Fix:** Use the same host character check as `of_host`, lowercase `from_host` before the PSL lookup, drop the fragment, and widen the tracker list (or keep only an allowlist of keys such as order and number).
- Fixed in c06a4ef8: the host must be `[a-z0-9.-]` as in `of_host`, `from_host` is lowercased before the PSL lookup, the fragment is dropped, and the tracker list covers `fbclid`, `gclid`, `_hs…`, `mkt_tok`, `msclkid` and others.

### S-11 — A guessed item name could tie a mail to someone's order (low, fixed)

- **Where:** `record_mail` in `crates/uwumail-store/src/shipments.rs`, the fallback for split orders.
- **Cause:** a shipping mail without a matching order number joins an open order of the same shop whose
  subject quotes the same item. An item name is easier to guess than an order number.
- **Fix:** the fallback only applies to authenticated mail; it is bound to the account and the shop,
  open orders of the last 30 days, and the item is a bound SQL parameter.

### S-12 — DPD, Hermes and GLS were asked through unofficial pages (info, removed)

- **Where:** `crates/uwumail-smtp/src/parcels.rs`.
- **Cause:** their public tracking pages are no API, change without notice and were already failing.
- **Fix:** removed with the `shipments.lookup.unofficial` setting; only DHL, UPS and FedEx are asked
  through their official APIs.

## What held up

- **Carrier requests:**
  - Numbers are checked (1–40 characters, ASCII letters, digits and `-`) before any URL is built, so nothing can be injected into a carrier URL.
  - Carrier hosts are fixed, and only public https is allowed through `check_url`.
  - Redirects are not followed. Answers are limited to 1 MiB and requests to 10 seconds.
  - Keys and tokens go in headers, never in URLs or logs.
  - `lookup_error` only ever holds the three constants `notFound`, `unavailable` and `rateLimited`.
- **Carrier parsers:** Indexing into `serde_json::Value` cannot panic. Event text and locations are clipped in `record_shipment_lookup`.
- **Account isolation:**
  - `record_shipment_lookup`, `claim_shipment_refresh` and `forget_junk_mail` all filter on `account_id`. `forget_junk_mail` removes only events with source `mail`/`model` of that one mail.
  - `Shipment/refresh` is not in the shared-account method list, so it only works in the caller's own account.
  - Both the admin switch and the person's `shipmentLookup` preference are checked, in the JMAP call and in the background query.
- **Spam-Netz:**
  - The key is checked (`usn_` prefix, its length and characters), sealed at rest and bound to the hub URL. It is dropped on a 401.
  - Scores are clamped to 0..4 per kind and 6 in total. Non-finite scores and unparsable expiry times are skipped.
  - Kinds and value lengths are filtered, and the cursor is URL-encoded.
  - Own domains and authenticated contacts are never reported, and neither are addresses or recipients.
  - Fetching and reporting are off by default, and the admin sync is limited to once a minute.
- **Egress:** the new `spamnet` and `parcels` routes default to direct and use the same checked exchange as the other purposes. The new secrets are `SettingKind::Secret` in the admin settings.
- **Shop link:** it is stored only from authenticated mail, at most 2 KiB, overwritten only by newer authenticated mail, and never fetched by the server.
