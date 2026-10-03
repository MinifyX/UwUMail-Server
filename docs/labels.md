# Labels

How the server puts labels on mail by itself: the eight **base labels** every person has, the
label's rules, the built-in detectors, learned senders, similar mails and the classifier, and the
model only for what they leave in doubt. What labels are and how they are managed over JMAP is in
[jmap-assist.md](jmap-assist.md#labels); how Sieve rules see them in [sieve.md](sieve.md#labels).

All of the deciding is in one crate, `crates/uwumail-labels`, which depends on no other UwUMail
crate (only on `mail-parser`, and only for its `parse` feature). The UwUMail app copies it as it is
for the labels of its other accounts, so the app and the server decide alike: same word lists,
same thresholds. This document describes what it does; the word lists themselves are in
`detect.rs` and `facts.rs`, and the crate's tests hold realistic German and English samples of
each detector and a corpus of 234 invented mails (see [Measuring](#measuring)).

The rule above all: **rather no label than a wrong one.** A mail gets at most a main label and a
second one, and none when nothing is sure enough.

## Base labels

Every person has these eight labels, created the first time labels are looked at (a mail is
delivered, `AssistLabel/get`). Each has a fixed definition with examples of what belongs in it and
of what looks alike but does not; the definitions do not overlap.

| `base` | German | English | What belongs in it |
| --- | --- | --- | --- |
| `invoice` | Rechnung | Invoice | money owed or paid: invoice, receipt, payment confirmation or reminder, credit note, a debit with its amount |
| `shipping` | Versand | Shipping | ordered goods on their way: order confirmation, shipped, tracking, delivery, pickup, returns |
| `appointment` | Termin | Appointment | a fixed date to go to or take part in, its confirmation, reminder, rescheduling or cancellation |
| `newsletter` | Newsletter | Newsletter | regular issues of subscribed content: news, digests, blog posts, project or club updates |
| `account` | Konto & Sicherheit | Account & security | the person's account at a service: sign-up, confirming the address, login codes, password reset, new sign-in, security alerts, changes of plan, terms or privacy policy |
| `personal` | Persönlich | Personal | written personally by a private person: friends, family, acquaintances |
| `work` | Arbeit/Geschäftlich | Work & business | written by a person in a professional context: colleagues, customers, partners, applications, authorities |
| `advertising` | Werbung | Promotions | mainly meant to sell: offers, discounts, sales, coupons, review requests |

French, Dutch, Japanese and Chinese names exist too; the definitions are German for German and
English otherwise. The language is the person's (`preferences.language`), else the server's.

- **Exclusions.** Some never go on one mail together: `personal` and `work` go with nothing but
  `appointment` (and not with each other); `newsletter` and `advertising` exclude each other and
  `invoice`, `shipping`, `account` and `appointment`; `account` excludes `shipping` and
  `appointment`.
- **Adopted labels.** A label the person had before with the same meaning (`Rechnungen`,
  `Invoices`, `Bestellungen & Versand`, `Termine`, `Persönlich` …) becomes the base label instead of
  a second one: it keeps its name, keyword and color and gets the definition; a detector that is
  the base label's own is dropped.
- **Switched one by one.** `auto: false` keeps a label from being put on by itself (by any of the
  ways below or the model); the person may still put it on by hand. The definition can't be
  changed, the name and color can. A deleted base label stays deleted until it is made again
  (`AssistLabel/set` create `{ "base": "invoice" }`). Base labels do not count toward the 30 own
  labels.
- The person's own labels stay as they are, next to the base labels.

## How a label is chosen

For each label that has `auto` on and is not on the mail yet, the surest of these ways gives a
**candidate** with a confidence from 0 to 1:

| Way | `source` | Confidence |
| --- | --- | --- |
| the label's `rules` match | `rule` | 1.0 (the other ways are not looked at) |
| a learned sender ([below](#learned-senders)) | `sender` | 0.9, from 5 hand-labelings 0.95 |
| the label's detector, or its base label's | `detector` | the detector's own, 0.7 to 0.95 ([Detectors](#detectors)) |
| similar mails ([below](#similar-mails)) | `similar` | 0.8 to 0.95 when sure, below 0.75 otherwise |
| the classifier ([below](#classifier)) | `classifier` | 0.9 |
| the model ([below](#asking-the-model)) | `ai` | 0.85, 0.92 with a hint of another way |

When the person took the label off a mail of the same sender by hand, no way but the rules puts it
on mail of that sender again. A bounce (a mail server's delivery report) gets no base label.

Then the labels are **chosen**, surest first (equally sure: rule, sender, detector, similar,
classifier, model; then the order of the labels):

1. the **main label** needs at least **0.8** (`MAIN_THRESHOLD`);
2. a **second label** needs at least **0.88** (`SECOND_THRESHOLD`), must not be excluded by the main
   one and must not come from the same detector;
3. never more than **two** (`MAX_LABELS`). Labels on the mail already count: with one there only a
   second may come, with two none.

## When

**At delivery,** for every recipient on this server whose mail is not going to Junk and who has
`nonAiLabels` on (the default), before the person's Sieve script runs: rules, detectors, learned
senders and the classifier. Delivery gives the whole step one second and stores the mail without
these labels when it takes longer or anything fails.

**Afterwards,** when the person has AI labels on (`autoLabels`), the label worker looks at the mail
again: the same ways plus similar mails, then the model only for the labels still in doubt
([Asking the model](#asking-the-model)). It never takes a label off. With `nonAiLabels` off, what
the cheap ways find is only a hint for the model (at most 0.79), never a label by itself.

## The mail as the deciding sees it

| | |
| --- | --- |
| `from` | the first address of `From`, lower case, at most 1,000 characters |
| `from_name` | its display name |
| `to` | the addresses of `To` and `Cc`, lower case, at most 50 |
| `subject` | the subject, at most 1,000 characters |
| `text` | the text: the `text/plain` body, or the HTML body turned into text (also when a "text" part holds HTML), at most 100,000 characters |
| `attachments` | name (may be empty) and content type of the first 100 attachment parts, each at most 1,000 characters |
| `has_attachment` | there is at least one attachment part |
| `calendar` | a `text/calendar` or `application/ics` part, or an attachment whose name ends in `.ics` |
| `headers` | the names and values (at most 1,000 characters) of `List-Unsubscribe`, `List-Unsubscribe-Post`, `List-Id`, `List-Post`, `Precedence`, `Auto-Submitted` and `X-Auto-Response-Suppress` |
| `from_trusted` | whether the `From` address says who sent the mail (see [Learned senders](#learned-senders)) |
| `known_sender` | the address is in one of the person's address books, or they wrote to it (among their 2,000 newest sent mails) |

**Folding** makes text comparable: lower case (Unicode), and every run of white space one space.

## Facts

Before anything decides, plain facts are read from the mail (`Facts`); the detectors use them, and
the model gets them as they are and is told to rely on them:

- **sender type**: `noReply` (`noreply`, `no-reply`, `mailer-daemon`, `notifications`, `server`,
  `cron` …), `marketing` (`newsletter@`, `news@`, `marketing@`, `angebote@`, or a sub-domain like
  `news.` or `email.`), `role` (`info@`, `support@`, `billing@` …, or a display name with a company
  word), `person` (any other address at a freemail provider; elsewhere a name from the display name
  in the address, or `first.last`), else `unknown`;
- **freemail** sender, **same domain** as a recipient (a colleague), **known** sender,
  **authenticated** (SPF, DKIM or DMARC vouch for `From`);
- **List-Unsubscribe**, **bulk** (`List-Id`, `Precedence: bulk/list`, `List-Unsubscribe-Post`),
  **discussion list** (`List-Post`), **automatic** (`Auto-Submitted` other than `no`), **bounce**;
- **amounts** of money, **invoice numbers** (after `Rechnungsnummer`, `invoice no.` …),
  **tracking** number and **carrier**, **dates**, **times**, a **calendar** part, a **PDF**,
  a **one-time code** (right after `Code`, `PIN`, `TAN`, `Sicherheitscode` …);
- **sales words** (discounts, `% off`, coupons, "nur heute", review requests …), **account words**
  in the subject (password, sign-in, verify, terms …), a **casual** greeting ("Hi", "LG", "Cheers")
  and a **formal** one ("Sehr geehrte", "Kind regards").

A mail is a **mass mail** when it has List-Unsubscribe, is sent in bulk or comes from a marketing
sender; it is **written by a person** when the sender type is `person` and it is no mass mail, not
automatic and not to a discussion list.

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

Word lists are matched in the folded text as **substrings** unless marked "word" in the source
(then the match must start and end at a word boundary). German compounds (`Mobilfunkrechnung`,
`Arzttermin`) are why most are substrings. Each detector answers a confidence; every one that
needs a company's mail loses 0.1 to 0.15 when nothing vouches for the sender (phishing copies
exactly these mails).

| Detector | Finds | Confidence |
| --- | --- | --- |
| `invoice` | a PDF whose name holds an invoice stem (`rechnung`, `invoice`, `quittung`, `beleg`, `mahnung`, `receipt`, `refund` …) | 0.95 |
| | an invoice stem in the subject with a PDF, or with an amount and an amount word (`betrag`, `total`, `fällig` …) | 0.9 |
| | an invoice stem in the subject and an amount, not from a person | 0.82 |
| | an invoice number and an amount and an invoice stem in the text, not from a person, no order words in the subject | 0.85 |
| | never for a mass mail with two sales words | |
| `appointment` | a calendar part | 0.95 |
| | an appointment stem in the subject (`termin` but not `liefertermin`, `einladung`, `meeting`, `reservierung`, `booking` …) and a date and a time | 0.88, 0.8 for a mass mail |
| | never for a mass mail with sales words (an advertised event) | |
| `newsletter` | List-Unsubscribe, no discussion list, none of invoice, shipping, appointment, account and advertising, and `List-Id`, `Precedence`, `List-Unsubscribe-Post` or a marketing sender; an editorial word (`newsletter`, `weekly`, `digest`, `ausgabe` …) in the subject or the first 600 characters | 0.88 |
| | the same without an editorial word and without sales words | 0.75 (a hint, never a label) |
| `shipping` | a tracking number that names its carrier (`1Z…` UPS, `TBA…` Amazon, `JJD…`/`00340434…` DHL), or digits next to a named carrier | 0.95 |
| | a carrier and a shipping word in the subject, no List-Unsubscribe | 0.85 |
| | an order word in the subject (`bestellung`, `versandbestätigung`, `your order`, `geliefert` …), from no person, no sales words, and goods and order words in the text (an order of a subscription or a download ships nothing) | 0.85, 0.8 with List-Unsubscribe |
| | never for a mass mail with two sales words | |
| `account` | a strong account word in the subject (`passwort`, `anmeldung`, `sign-in`, `verify`, `security alert` …) | 0.9 |
| | a weaker one (`dein konto`, `welcome to`, `membership`, `vertrag` …) with a code or a second word | 0.85 |
| | a weaker one alone, from a no-reply sender, no mass mail | 0.8 |
| | a one-time code from a no-reply or role sender with a sign-in or confirm word, no mass mail | 0.85 |
| | never from a person, to a discussion list, with two sales words, or a weak word with an amount ("your account was credited") | |
| `personal` | written by a person from a freemail address, not formal, no sales words, none of invoice, shipping and account: a known sender with a casual greeting / a casual greeting / a known sender | 0.9 / 0.85 / 0.82 |
| `work` | written by a person, no freemail, no sales words, not to oneself: from the person's own domain / formal from a known sender | 0.9 / 0.85 |
| `advertising` | a mass mail, no discussion list, no tracking number; score = sales words + those in the subject: 4 or more / 3 / 2 with one in the subject | 0.92 / 0.86 / 0.82 |

`params` name what was found: `{ "attachment" }` or `{ "word", "amount" }` or `{ "number",
"amount" }` (invoice), `{ "calendar": true }` or `{ "word", "date", "time" }` (appointment),
`{ "header", "word" }` (newsletter), `{ "carrier", "tracking", "word"? }` (shipping), `{ "word",
"code" }` (account), `{ "known", "freemail" }` (personal), `{ "colleague", "known" }` (work),
`{ "words" }` (advertising).

## Learned senders

For each label and From address the server counts how often the person gave a mail from that
address the label **by hand** (JMAP `Email/set`, IMAP `STORE`, so every mail app teaches it).
From a count of **2** on, new mail from the address gets the label: `{ "address", "count" }` — but
only when the `From` address says who really sent it: at delivery the server takes it when SPF,
DKIM or DMARC vouch for the address, since anyone can write a known address into `From` and so get
a label, and whatever Sieve rule sorts by it, onto their mail (the label worker takes DMARC, or
DKIM and SPF together). With the sender checks switched off (`smtp.verify_senders = false`)
nothing vouches for any address, so no learned sender puts a label on then. Apps that cannot tell
keep `from_trusted` on.

Taking the label off a mail of that address by hand (also with `AssistLabel/undo`) turns the count
into **−1**: from then on no way but the label's rules puts the label on mail of that sender (a
correction is the clearest thing a person says). Putting it on by hand again starts counting at 1.

Only labels with `learnSenders` on count senders. At most 5,000 addresses are kept per label; a new
one beyond takes the place of the least counted (by amount). Addresses over 320 characters are not
learned.

Only changes the person makes count: labels the server or the AI put on, keywords Sieve sets at
delivery, and taking a label off every mail when it is destroyed teach nothing. Neither does
labeling by someone a folder is shared with (the owner's labels learn from the owner), except in a
shared mailbox, whose labels are all its members'. While labels without a model are switched off
(`nonAiLabels`), senders and the classifier learn nothing (the model's corrections still do,
[below](#corrections-for-the-model)).

## Similar mails

The person's labeled mails (the classifier's examples, [below](#classifier)) are compared with a
new mail, and the labels of the most alike vote (k nearest neighbours, `similar.rs`):

- **With an embeddings provider** (the admin adds one: OpenAI, Ollama or any OpenAI-compatible
  `/embeddings`, see [llm.md](llm.md#embeddings)), the first 2,000 characters of subject, sender
  domain and text are embedded (`classification: ` in front for nomic-embed-text). Vectors
  are kept small: normalized to length 1, one signed byte per dimension and one scale, so 768
  dimensions take 772 bytes, at most 4,096 dimensions, one per labeled mail and model. They go
  with the mail (destroyed) and the account. Similarity is the cosine; neighbours below **0.55**
  are left out, a label is sure only with a neighbour at least **0.78** alike.
- **Without one**, or when it fails, the mails' token sets ([Classifier](#classifier)) are compared
  by Jaccard similarity: below **0.12** left out, sure from **0.35**.

The **7** most alike vote, weighted by similarity. A label is sure when at least 3 of them have
it, they hold at least 60 % of the weight and the best is at least the sure similarity: confidence
`0.8 + 0.15 × (share − 0.6) / 0.4` (at most 0.95). Otherwise its confidence is `0.75 × share`, a
hint for the model. `params`: `{ "neighbours", "similarity" }`.

Each time the worker labels a mail it embeds up to 8 labeled mails that have no vector yet along
with it, in the same request, so the vectors fill up without a separate job. Similar mails are
used by the label worker (with AI labels on), not at delivery, which has one second.

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

Only labels with `classifier` on learn examples. The server learns a hand-labeling a moment later,
from a queue: an email waits there once per label (putting a label on and off again leaves only
the last change), at most 500 per person, and people take turns.

For label L, the examples split into `P` (with L) and `N` (all other examples). For every token the
model counts in how many examples of `P` and of `N` it occurs (`p(t)`, `n(t)`).

**Deciding.** Nothing happens before `|P| ≥ 15` and `|N| ≥ 15`. Then, for the mail's tokens that
occur in at least 2 examples (`p(t) + n(t) ≥ 2`; counts below 0 read as 0):

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

## Asking the model

The worker asks the model only about the labels in doubt (`ask_about`):

- none when two labels are sure already (or on the mail);
- with a main label sure or on the mail, only the person's own labels (the base labels' detectors
  have had their say), never one excluded by a label there;
- otherwise every label with `auto` on that is not on the mail.

The prompt has the labels (a base label with its definition, examples and counter-examples; an own
label with its description), the [facts](#facts), what the cheap ways found but were not sure of
(as hints), up to 12 of the person's [corrections](#corrections-for-the-model) and the mail (as
data, never orders). The model gives a reason first and then `"fits": "yes"`, `"no"` or
`"unsure"`; "unsure" counts as no.

A yes counts **0.85** (`AI_YES`): enough for a main label, not for a second one. With a hint of at
least 0.5 from another way it counts **0.92** (`AI_SUPPORTED`). Then the facts may still rule it out
(`ruled_out`): `personal` needs a mail written by a person and not from the person's domain;
`work` no mass mail, nothing automatic, no no-reply or marketing sender and no role address
(`info@` …) the person does not know; `newsletter` and `advertising` a mass mail that is not to a
discussion list; `account` no mail written by a person, and an account word in the subject or a
one-time code (notices of apps and devices are no account mail); `shipping` no mail a person wrote
from a freemail address; a bounce gets nothing. A
model that says yes to more than two labels, or to two that exclude each other, is not believed
at all. The model's candidates and the others are then [chosen](#how-a-label-is-chosen) together.

When no model can be used, or it fails after the cheap ways were sure of something, their labels
go on alone.

These rules come from the labels people took off again in 0.21: most were "personal" on company or
no-reply mail that greets the reader by name, "newsletter" on account or shipping notices, and
answers that put four labels on one mail.

## Corrections for the model

Each label put on or taken off by hand (while AI labels are on) is kept as an example for the
model: the sender's domain (never the address), the subject (at most 120 characters) and the start of the text (at most
200), the newest 4 positive and 3 negative per label. The prompt shows those of the labels asked
about, at most 12. They go with the label and the mail.

## Overlapping labels

Overlapping labels are what puts two labels on one thing, or the wrong one of two.
`AssistLabel/checkOverlap` (and the webmail while a label is written) tells, for a name and a
description, which labels it overlaps with (`overlap.rs`):

- `name`: the same name (compared as stems: folded, umlauts and common endings off), or a name of
  the same base label;
- `meaning`: a word of the name, or two of the description, is what a base label is about
  (`Handyrechnungen` → Rechnung; compounds count);
- `words`: at least two words in common with another label's name and description, and at least a
  quarter of all.

## Reasons

Each label set this way gets a log entry whose `reason` is an English sentence made from `code`
and `params` (apps translate the code themselves); the worker adds `confidence` to `params`:

| `code` | `reason` |
| --- | --- |
| `rule` | `Matches the label's rules: subject contains "Rechnung"` (the matched conditions joined with `and` or `or`, by `match`) |
| `sender` | `leni@example.org got this label by hand 3 times` |
| `invoice` | `Looks like an invoice: PDF attachment "Rechnung_4711.pdf"` / `Looks like an invoice: "Rechnung" in the subject, 49,90 €` / `Looks like an invoice: invoice number RE-4711, 49,90 €` |
| `appointment` | `Looks like an appointment: a calendar invitation` / `Looks like an appointment: "Termin" on 06.10.2026 at 09:30` |
| `newsletter` | `Looks like a newsletter: it has List-Unsubscribe and List-Id, and "newsletter"` |
| `shipping` | `Looks like a shipment: DHL, tracking number 00340434161234567890` (parts left out when `null`) / `Looks like a shipment: "Bestellung" in the subject` |
| `account` | `Looks like a message about your account: "passwort" in the subject` / `… a one-time code` |
| `personal` | `Looks personal: written by a person from a sender you know` |
| `work` | `Looks like work: written by a colleague from your own domain` |
| `advertising` | `Looks like advertising: rabatt, gutschein` |
| `similar` | `Like 5 of your mails with this label (91 % alike)` |
| `classifier` | `Similar to the 23 mails with this label (99.4 % sure)` |
| `ai` | the model's reason |

## Measuring

`crates/uwumail-labels/tests/corpus/mails.jsonl` holds 234 invented mails (121 German, 113
English; reserved domains only) with the base labels they should get; its format is in the
`README.md` next to it. A CI test (`base_labels_without_a_model_are_precise`) decides them without
a model and nothing learned and requires a precision of at least 90 % over all, 80 % per label, a
recall of at least 50 %, never more than two labels and never two that exclude each other. It
prints a table (`cargo test -p uwumail-labels corpus -- --nocapture`; `UWUMAIL_SHOW_MISSED=1` lists
the missed labels too).

With a model, `cargo test -p uwumail-assist --test integration eval -- --ignored --nocapture`
compares 0.21 (every label asked, every yes believed) with 0.22 (model in doubt; with similar mails
when an embeddings endpoint is given), see the test's docs for its variables. A local corpus of
real mails (never in the repository) can be added with `UWUMAIL_REAL_CORPUS`.

