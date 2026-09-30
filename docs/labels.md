# Labels without a model

How the server puts labels on new mail by itself, without asking an AI provider: the label's
rules, the built-in detectors, learned senders and the classifier. What labels are and how they
are managed over JMAP is in [jmap-assist.md](jmap-assist.md#labels); how Sieve rules see them in
[sieve.md](sieve.md#labels).

All of the deciding is in one crate, `crates/uwumail-labels`, which depends on no other UwUMail
crate (only on `mail-parser`, and only for its `parse` feature). The UwUMail app copies it as it is
for the labels of its other accounts, so the app and the server decide alike: same word lists,
same thresholds. This document describes what it does; the crate's tests hold realistic German and
English samples of each detector.

## When

At delivery, for every recipient on this server whose mail is not going to Junk and who has at
least one label and `nonAiLabels` on (the default), before the person's Sieve script runs. Each
label is looked at once; the first of these four that matches puts it on, and it is logged with
its `source`, `code` and `params`:

1. `rules` (source `rule`),
2. `detector` (source `detector`, `code` the detector's name),
3. a learned sender, when `learnSenders` is on (source `sender`),
4. the classifier, when `classifier` is on (source `classifier`).

A label whose keyword is on the mail already is skipped (the crate takes the keywords the mail
has; at delivery there are none yet). Delivery gives the whole step one second and stores the mail
without these labels when it takes longer or anything fails.

Afterwards, when the person has AI labels on, the model judges only the labels that are still
missing, and never takes one off.

## The mail as the deciding sees it

| | |
| --- | --- |
| `from` | the first address of `From`, lower case |
| `subject` | the subject |
| `text` | the text: the `text/plain` body, or the HTML body turned into text, at most 100,000 characters |
| `attachments` | name (may be empty) and content type of every attachment part |
| `has_attachment` | there is at least one attachment part |
| `calendar` | a `text/calendar` or `application/ics` part, or an attachment whose name ends in `.ics` |
| `headers` | the names and values of `List-Unsubscribe`, `List-Unsubscribe-Post`, `List-Id`, `List-Post` and `Precedence` |

**Folding** makes text comparable: lower case (Unicode), and every run of white space one space.

## Rules

`match` `all`: every condition must match; `any`: at least one. No conditions never match.

| Field | Matches when |
| --- | --- |
| `from` | `value` (folded) contains `@` after its first character: `from` equals it. Otherwise, without a leading `@`, it is a domain: `from`'s domain equals it or ends with `.` and it |
| `subject` | folded `subject` contains folded `value` |
| `text` | folded `text` contains folded `value` |
| `hasAttachment` | `value` is `true` and `has_attachment`, or `false` and not |

`params` are the conditions that matched: `{ "match": "any", "conditions": [{ "field": "subject",
"value": "Rechnung" }] }`.

## Detectors

Word lists are matched in the folded text as **substrings** unless marked "word": then the match
must start and end at a word boundary (the string's start or end, or a character that is neither a
letter nor a digit). German compounds (`Mobilfunkrechnung`, `Arzttermin`) are why most are
substrings.

### Invoice (`invoice`)

- **Invoice stems**: `rechnung`, `invoice`, `faktura`, `quittung`, `zahlungsbeleg`, `kassenbon`,
  `kaufbeleg`, `gutschrift`, `zahlungsbestätigung`, `receipt`, `payment confirmation`,
  `billing statement`, `credit note`.
- **Amount**: a number with exactly two decimals (`49,90`, `1.249,00`, `12.50`, `1,249.00`) right
  before or after a currency (`€`, `eur`, `$`, `usd`, `£`, `gbp`, `chf`; at most one space
  between them).
- **Amount words**: `betrag`, `summe`, `gesamt`, `total`, `amount`, `zu zahlen`, `fällig`, `due`,
  `mwst`, `ust`, `vat`, `netto`, `brutto`.

It matches when

1. an attachment's name ends in `.pdf` and contains an invoice stem → `{ "attachment": name }`, or
2. the subject contains an invoice stem, and there is a PDF attachment (name ending in `.pdf` or
   type `application/pdf`) or the text has an amount and an amount word → `{ "word": the word of
   the subject that holds the stem, as written, "amount": the first amount of the text as written,
   or null }`.

### Appointment (`appointment`)

- **Appointment stems**: `termin` (but not as part of `liefertermin` or `zustelltermin`),
  `appointment`, `einladung`, `invitation`, `meeting`, `reservierung`, `reservation`, `buchung`,
  `booking`, `sprechstunde`.
- **Date**: `31.12.2026`, `31.12.26`, `31.12.` (followed by a space), `2026-12-31`, or a day
  number (1–31, with an optional `.`) next to a month name (`januar`…`dezember`, `jan`…`dez` with
  `mär`/`mrz`, `january`…`december`, `jan`…`dec`), in either order.
- **Time**: `9:30`/`09:30` (hours 0–23, minutes 00–59), or a number 0–23 followed by `uhr`, `h`,
  `am` or `pm` (with or without a space).

It matches when

1. there is a calendar part or `.ics` attachment → `{ "calendar": true }`, or
2. the subject contains an appointment stem and the subject and text together have a date and a
   time → `{ "word", "date", "time" }` as written.

### Newsletter (`newsletter`)

It matches when there is a `List-Unsubscribe` header, **no** `List-Post` header (that is a
discussion list), none of invoice, shipping and appointment match (shops send those with
`List-Unsubscribe` too), and one of these is there: `List-Id` → `{ "header": "List-Id" }`,
`Precedence: bulk` or `list` → `{ "header": "Precedence" }`, `List-Unsubscribe-Post` →
`{ "header": "List-Unsubscribe-Post" }` (the first of these that is there).

### Shipping (`shipping`)

- **Tracking numbers** that name their carrier by themselves (in subject or text, as a whole
  word, ignoring case): `1Z` and 16 letters or digits (UPS); `TBA` and 12 digits (Amazon); `JJD`
  and 18 to 20 digits, or `00340434` and 12 digits (DHL).
- **Carriers** (in subject, text or `from`'s domain): `dhl`, `dpd`, `hermes`, `gls` (word),
  `amazon` (word), `deutsche post` → DHL; and `UPS` as a whole word **in capitals** in the original
  text (`ups` is too common in English words and exclamations).
- **Shipping words**: `versand`, `versendet`, `versandt`, `verschickt`, `sendung`, `paket`,
  `zustellung`, `lieferung`, `unterwegs`, `zugestellt`, `shipped`, `shipment`, `shipping`,
  `tracking`, `package`, `parcel`, `delivery`, `delivered`, `dispatched`.

It matches when

1. a tracking number that names its carrier is there → `{ "carrier", "tracking" }`, or
2. a carrier is named, a shipping word is in the subject or text, and a run of 10 to 20 digits
   stands alone as a word → `{ "carrier": the first carrier named, "tracking": the digits }`, or
3. a carrier is named, a shipping word is in the subject and there is no `List-Unsubscribe`
   header (a shop's newsletter about free shipping names both too) → `{ "carrier", "tracking":
   null }`.

"The first carrier named" goes in this order: DHL, DPD, Hermes, GLS, UPS, Amazon.

## Learned senders

For each label and From address the server counts how often the person gave a mail from that
address the label **by hand** (JMAP `Email/set`, IMAP `STORE`). Taking the label off a mail of that
address by hand (also with `AssistLabel/undo`) forgets the address for the label (the count is
gone). From a count of **2** on, new mail from the address gets the label: `{ "address", "count" }`.

Only changes the person makes count: labels the server or the AI put on, keywords Sieve sets at
delivery, and taking a label off every mail when it is destroyed teach nothing.

## Classifier

A naive Bayes model per person and label, learned from the person's own hand-labeling.

**Tokens** of a mail, each taken once, at most 400, in this order:

1. `from:` and the `from` address, `domain:` and its domain;
2. the words of the subject, each with `subject:` in front;
3. the words of the first 20,000 characters of the text.

A word is a run of letters and digits (Unicode), folded to lower case, 3 to 24 characters long,
not only digits. (The server keeps tokens as 64-bit FNV-1a hashes of their UTF-8 bytes; that is a
matter of storage, not of the method.)

**Examples.** The person's examples are the mails they labeled or unlabeled by hand, plus one
recent mail per labeling:

- a label put on by hand: the mail is an example **with** that label;
- a label taken off by hand: the mail is an example **without** it;
- each time a label is put on by hand, one more mail is learned as an example without any label:
  of the 200 newest mails in the inbox from the last 60 days that carry no label and are not
  examples yet (newest first, counted from 0), the one at position `id mod n`, where `id` is the
  server's number of the mail just labeled and `n` how many there are (at most 200). This is what
  ordinary mail looks like; without it a person with a single label would never have examples
  without it. Such a mail that is later labeled by hand simply becomes an example with the label;
- at most 3,000 examples per person; beyond that the oldest are forgotten.

For label L, the examples split into `P` (with L) and `N` (all other examples). For every token the
model counts in how many examples of `P` and of `N` it occurs (`p(t)`, `n(t)`).

**Deciding.** Nothing happens before `|P| ≥ 15` and `|N| ≥ 15`. Then, for the mail's tokens that
occur in at least 2 examples (`p(t) + n(t) ≥ 2`):

```
w(t) = ln( (p(t) + 1) / (|P| + 2) ) − ln( (n(t) + 1) / (|N| + 2) )
```

The 40 tokens with the largest `|w(t)|` (ties: in token order) are summed with a prior that never
favours the label:

```
logit = min(0, ln(|P| / |N|)) + Σ w(t)
probability = 1 / (1 + e^(−logit))
```

The label goes on when `probability ≥ 0.99` and at least 3 of the summed tokens have `w(t) > 0`:
`{ "probability": cut to 3 decimals, "examples": |P| }`.

## Reasons

Each label set this way gets a log entry whose `reason` is an English sentence made from `code`
and `params` (apps translate the code themselves):

| `code` | `reason` |
| --- | --- |
| `rule` | `Matches the label's rules: subject contains "Rechnung"` (the matched conditions joined with `and` or `or`, by `match`) |
| `sender` | `leni@example.org got this label by hand 3 times` |
| `invoice` | `Looks like an invoice: PDF attachment "Rechnung_4711.pdf"` / `Looks like an invoice: "Rechnung" in the subject, 49,90 €` |
| `appointment` | `Looks like an appointment: a calendar invitation` / `Looks like an appointment: "Termin" on 06.10.2026 at 09:30` |
| `newsletter` | `Looks like a newsletter: it has List-Unsubscribe and List-Id` |
| `shipping` | `Looks like a shipment: DHL, tracking number 00340434161234567890` (parts left out when `null`) |
| `classifier` | `Similar to the 23 mails with this label (99.4 % sure)` |
