# JMAP extension: AI assistant

UwUMail Server can ask a language model for help with mail: write or rewrite a
message, summarize a mail or a conversation, give a second opinion on whether a
mail is spam, read appointments out of a mail, and put the person's own labels
on incoming mail. Which models are there, who may use them and how much is
decided by the admin and the person; see [llm.md](llm.md) for the providers,
the admin's settings and what leaves the server.

Every call to a model is made **by the server**, never by the browser. That is
what lets labels be set in the background, keeps the keys out of the browser,
and lets the admin count and limit what is used.

Labels are the exception to "a model": they are the person's own and work without any
provider. The server sets them on new mail by rules, built-in detectors, learned senders and a
small classifier of its own before any model is asked (see [Labels](#labels) and
[labels.md](labels.md)).

The model never acts on its own. Mail content is handed to it as data, the
answers are either plain text the person looks at before anything happens, or
JSON of a fixed shape that the server checks: a label that is not one of the
person's labels, an event without a date or a verdict that is not one of four
words is dropped. The model gets no tools.

## Capability

`urn:uwumail:jmap:assist` in the session's `capabilities`:

```json
"urn:uwumail:jmap:assist": { "streamUrl": "https://mail.example.org/jmap/assist/stream" }
```

and in the `accountCapabilities` of the person's own account (never of an
account others share with them):

```json
"urn:uwumail:jmap:assist": {
  "features": {
    "compose": true,
    "summarize": true,
    "spamCheck": true,
    "extractEvents": true,
    "autoLabels": true
  },
  "mayAddProviders": true,
  "mayUsePrivateAddresses": false,
  "maxProviders": 10,
  "maxLabels": 30,
  "maxLabelConditions": 10,
  "maxInstructionChars": 2000,
  "maxTextChars": 20000,
  "foreignMail": false
}
```

| Property | |
| --- | --- |
| `features` | per feature, whether this person can use it right now: the admin allows it and some provider the person may use is allowed it. `autoLabels: true` means it *can* be switched on; whether it is on is in [`AssistSettings`](#assistsettings). |
| `mayAddProviders` | the admin lets people add providers with their own keys |
| `mayUsePrivateAddresses` | such a provider may point into the local network (Ollama on the LAN) |
| `maxProviders` | how many providers of their own one person may have |
| `maxLabels` | how many labels one person may have |
| `maxLabelConditions` | how many conditions the `rules` of one label may have |
| `foreignMail` | the admin lets this person use the assistant for mail of **other** accounts (Exchange, Gmail, IMAP in the UwUMail app): the calls of [Foreign mail](#foreign-mail) accept mail content the client sends. `false` by default |
| `maxInstructionChars` | longest `instruction` of `Assist/compose` |
| `maxTextChars` | longest `text` of `Assist/compose`; mail content is cut to about this much too |

The session `state` changes when `features`, `foreignMail` or the two `may…`
flags change, so a client fetches the session again and sees the new capability.

The capability is there for the person's own account whenever the server has
the assistant, also when no provider can be used: [labels](#labels) need no
model. `AssistLabel/get`, `/set`, `/log` and `/undo` and `AssistSettings`
work then; everything that asks a model answers `assistUnavailable`.

A client adds the capability to `using` to call any method below.

All methods work in the person's own account only. Called with the id of a
shared account they answer `accountNotSupportedByMethod`.

## Common errors

| `type` | When |
| --- | --- |
| `assistUnavailable` | The feature is switched off by the admin, or no provider the person may use is allowed it. |
| `overQuota` | The daily limit of the provider this feature uses is reached (requests or tokens); `description` says which and when it resets (midnight UTC). |
| `providerFailed` | The provider did not give a usable answer: wrong key, not reachable, no answer in time, too busy (HTTP 429), an answer that is not the requested shape, or a refusal. `description` says which in plain words; for 429 a `retryAfter` (seconds) is added when the provider named one. |
| `notFound` | The email, thread, provider or label is not in the account. |
| `forbidden` | Changing a provider that is not the person's own. |
| `accountNotSupportedByMethod` | Called for a shared account. |
| `invalidArguments` | as in RFC 8620 |

## AssistProvider

A way to reach a model: one the admin set up for the server, or one the person
added with their own key.

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | server-set, like `q12` |
| `name` | `String` | shown in pickers, 1 to 60 characters |
| `kind` | `String` | `openai`, `anthropic`, `gemini`, `mistral`, `openrouter`, `ollama`, `openaiCompatible` or `chatgpt` |
| `scope` | `String` | `server` (set up by the admin) or `personal` (the person's own) |
| `baseUrl` | `String\|null` | where it is reached: always for `ollama` and `openaiCompatible`, for `openai` and `anthropic` only when it was changed (a proxy or gateway); `null` for server providers (the admin's addresses are not shown) and for kinds with a fixed address |
| `apiKey` | `String` | write-only: never returned. Omitted in an update, the stored key stays; `""` removes it |
| `hasKey` | `Boolean` | a key is stored |
| `keyHint` | `String\|null` | the last four characters of the key, like `…a1b2` |
| `model` | `String\|null` | the model for writing (compose) |
| `fastModel` | `String\|null` | the cheaper, faster model for summaries, spam checks, events and labels; `model` when `null` |
| `features` | `String[]` | the features this provider may be used for (server providers: what the admin allowed; personal providers: all the admin allows) |
| `quota` | `Object\|null` | server providers with a daily limit: `{ "requestsPerDay": Number\|null, "tokensPerDay": Number\|null }` per person |
| `experimental` | `Boolean` | `true` for `chatgpt` |
| `connected` | `Boolean` | `chatgpt`: signed in; others: `true` when a key is stored or none is needed |
| `inputPricePerMillion` | `Number\|null` | US dollars per million tokens sent, set by hand; `null`: automatic, from the price lists ([llm.md](llm.md#costs)) |
| `outputPricePerMillion` | `Number\|null` | the same for the tokens of the answer (and the model's thinking) |
| `pricePerRequest` | `Number\|null` | US dollars per request on top of the tokens, set by hand; `null`: automatic |
| `price` | `Object\|null` | read-only: what the default model (`model`) costs in US dollars, the whole price sheet (below); `null` when not known |

`price`:

```json
{ "inputPerMillion": 0.25, "outputPerMillion": 2.0, "reasoningPerMillion": 2.0,
  "cacheReadPerMillion": 0.025, "cacheWritePerMillion": 0.25,
  "perRequest": 0, "perImage": 0, "webSearchPerQuery": 0,
  "tiers": [{ "aboveTokens": 200000, "inputPerMillion": 0.5, "outputPerMillion": 4.0,
              "reasoningPerMillion": 4.0, "cacheReadPerMillion": 0.05 }],
  "supportsReasoning": true, "maxOutputTokens": 128000, "source": "auto" }
```

- `reasoningPerMillion`: thinking tokens; the answer's price where the lists
  name none (and always with a price set by hand).
- `cacheReadPerMillion`, `cacheWritePerMillion`: prompt tokens the provider read
  from or wrote to its cache; the prompt's price where the lists name none.
- `perRequest`, `perImage`, `webSearchPerQuery`: US dollars per request, per
  picture sent to the model and per web search; `0` where none.
- `tiers`: higher prices once a request's prompt is larger than `aboveTokens`
  (LiteLLM's `*_above_128k_tokens`); the whole request is charged at them.
  Empty with a price set by hand.
- `supportsReasoning`, `maxOutputTokens`: what the lists say about the model:
  it thinks before it answers, and the most it writes at once (thinking
  included); `false`/`null` when they don't say.
- `source`: `auto` (the lists), `manual` (a price set by hand; what is not set
  comes from the lists), `free` (Ollama, a ChatGPT subscription: all `0`).

For a server provider whose costs the admin does not show to people,
`inputPricePerMillion`, `outputPricePerMillion`, `pricePerRequest` and `price` are `null`, and
so are the costs of `Assist/estimate` and `Assist/usage` for it. A person's own
providers always show them.

### AssistProvider/get

Standard `/get`. `ids: null` answers every provider the person may use: the
server's that are allowed for them (in the admin's order) and their own. The
`state` changes when any of them changes.

### AssistProvider/set

Standard `/set` for the person's **own** providers (`create`, `update`,
`destroy`); server providers answer `forbidden`. Creating needs
`mayAddProviders`. Properties that may be set: `name`, `kind` (create only),
`baseUrl`, `apiKey`, `model`, `fastModel`, `inputPricePerMillion`,
`outputPricePerMillion` (0 to 100,000, or `null` for automatic) and
`pricePerRequest` (0 to 100, or `null`).

`SetError` types: `forbidden` (the admin does not allow own providers, or it is
a server provider), `overQuota` (more than `maxProviders`), `invalidProperties`
with `properties` naming the field and `description` saying why: an unknown
kind, a `baseUrl` that is not `http(s)://`, carries a login or points into the
local network while `mayUsePrivateAddresses` is `false`, a `http://` address
that is not in the local network (keys never travel unencrypted over the
internet), a name that is empty or too long.

Destroying a provider that a feature choice in `AssistSettings` names drops
that choice.

### AssistProvider/models

Asks the provider which models it offers; doubles as a test of the key.

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `providerId` | `Id` | |

```json
["AssistProvider/models", {
  "accountId": "a1",
  "providerId": "q3",
  "models": [{ "id": "gpt-5-mini", "name": "gpt-5-mini" }],
  "model": "gpt-5-mini",
  "fastModel": "gpt-5-nano"
}, "0"]
```

`models` holds at most 500 entries, sorted by id; `model` and `fastModel` are
the provider's settings, or the kind's suggestion when it has none. A provider
without a list (ChatGPT) answers the models known to work. Errors:
`notFound`, `providerFailed`.

### AssistProvider/chatgptLogin, AssistProvider/chatgptPoll

**Experimental.** Signs a `chatgpt` provider in with a ChatGPT subscription
through OpenAI's device-code login, the way the Codex CLI does it. See the
warning in [llm.md](llm.md#chatgpt-subscription-experimental).

`AssistProvider/chatgptLogin { accountId, providerId }` answers
`{ accountId, providerId, userCode, verificationUri, interval, expiresAt }`:
the person opens `verificationUri`, signs in with ChatGPT and enters
`userCode`. The client then calls `AssistProvider/chatgptPoll
{ accountId, providerId }` every `interval` seconds until `status` is no longer
`pending`:

| `status` | |
| --- | --- |
| `pending` | not confirmed yet |
| `connected` | signed in; the tokens are sealed and stored with the provider |
| `expired` | the code ran out (15 minutes); start again |
| `failed` | `description` says why |

## AssistSettings

A singleton with the person's choices.

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | always `singleton` |
| `default` | `Choice\|null` | the provider (and model) every feature uses unless it has its own |
| `features` | `String[Choice\|null]` | per feature (`compose`, `summarize`, `spamCheck`, `extractEvents`, `autoLabels`), its own choice |
| `autoLabels` | `Boolean` | the **model** judges the labels of incoming mail (opt-in, `false` by default) |
| `nonAiLabels` | `Boolean` | labels are put on incoming mail by the label's rules, detector, learned senders and classifier, without a model ([Auto-labels](#auto-labels)); `true` by default |
| `effective` | `String[Effective\|null]` | read-only: per feature, what will really be used, or `null` when nothing can |

A `Choice` is `{ "providerId": Id, "model": String|null }`; `model: null` means
the provider's `model` for `compose` and its `fastModel` for everything else.
An `Effective` is `{ "providerId", "providerName", "model", "scope" }`. When a
choice names a provider the person may no longer use, the next one applies:
the feature's choice, the default, then the first allowed server provider,
then the first own provider.

`AssistSettings/get { accountId, ids }` and `AssistSettings/set { accountId,
update: { "singleton": { … } } }` work like `VacationResponse`. `SetError`
`invalidProperties` names a provider the person may not use for that feature.

Whether the webmail asks the model for events by itself when a mail opens is
the [user setting](jmap-settings.md) `assist.refineEvents` (default `false`).

## Assist/compose

Writes or rewrites a message. The answer is plain text that the client shows
as a preview; nothing is put into the draft until the person accepts it.

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `mode` | `String` | `write` (a new text from `instruction`), `rewrite` (`text` in the way `preset` says) or `adjust` (`text` changed as `instruction` says) |
| `instruction` | `String\|null` | what to write or change; needed for `write` and `adjust`; for `rewrite` an optional addition |
| `preset` | `String\|null` | for `rewrite`: `formal`, `casual`, `shorter`, `friendlier`, `clearer`, `proofread` (spelling and grammar only) or `translate` |
| `targetLanguage` | `String\|null` | for `translate`: a language name or tag (`English`, `fr`) |
| `text` | `String\|null` | the current draft as plain text; needed for `rewrite` and `adjust` |
| `subject` | `String\|null` | the draft's subject, as context |
| `replyToEmailId` | `Id\|null` | the mail being answered: its text (without quoted history, cut to size) is context |
| `foreignMails` | `ForeignMail[]\|null` | instead of `replyToEmailId`: the mail being answered, sent along ([Foreign mail](#foreign-mail)) |
| `wantSubject` | `Boolean` | `write` only: also propose a subject (default `false`) |
| `language` | `String\|null` | the person's UI language as a hint (`de`); the model otherwise answers in the language of the instruction or the mail |

Response: `{ accountId, text, subject, providerId, providerName, model, usage }`
with `subject` a proposal or `null`, and `usage` `{ inputTokens, outputTokens,
reasoningTokens }` as the provider reported them (`reasoningTokens`: what the
model spent thinking, not part of `outputTokens`; Anthropic counts thinking in
`outputTokens`).

## Assist/summarize

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id\|null` | one mail … |
| `threadId` | `Id\|null` | … or a whole conversation (at most its latest 20 mails, oldest first) |
| `foreignMails` | `ForeignMail[]\|null` | … or mail sent along ([Foreign mail](#foreign-mail)) |
| `language` | `String\|null` | the language to answer in (`de`); the mail's language otherwise |

Response: `{ accountId, emailId, threadId, summary, providerId, providerName,
model, usage }`. `summary` is plain text: one or two sentences, then up to
five lines starting with `- ` for the points that matter (what is asked of the
reader, dates, amounts).

## Assist/spamCheck

A second opinion on a mail the person is unsure about. The server's own
findings come along, so the client shows both, and the person decides with the
usual "Spam" / "Not spam" actions.

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id` | or `foreignMails` ([Foreign mail](#foreign-mail)) |
| `language` | `String\|null` | the language of `reasons` |

```json
["Assist/spamCheck", {
  "accountId": "a1",
  "emailId": "e42",
  "verdict": "phishing",
  "confidence": 0.9,
  "reasons": ["Asks to confirm a password through a link", "The link leads to a different domain than the sender's"],
  "signals": {
    "authentication": { "spf": "fail", "dkim": "none", "dmarc": "fail", "fromDomain": "bank.example" },
    "spamScore": 4.2,
    "spamThreshold": 5.0,
    "tests": ["DMARC_FAIL", "LINK_MISMATCH"],
    "inJunk": false,
    "sender": {
      "address": "service@bank.example",
      "earlierMessages": 0,
      "earlierInJunk": 0,
      "writtenTo": 0,
      "inContacts": false,
      "firstSeen": null
    }
  },
  "providerId": "q1", "providerName": "Mistral", "model": "mistral-small-latest",
  "usage": { "inputTokens": 1830, "outputTokens": 96, "reasoningTokens": 0 }
}, "0"]
```

| Field | |
| --- | --- |
| `verdict` | `legitimate`, `suspicious`, `spam` or `phishing` |
| `confidence` | 0 to 1, the model's own estimate |
| `reasons` | at most six short sentences |
| `signals.authentication` | SPF, DKIM and DMARC as this server's `Authentication-Results` recorded them (`pass`, `fail`, `softfail`, `neutral`, `none`, …), `null` each when the mail did not come from another server |
| `signals.spamScore`, `spamThreshold`, `tests` | the server's spam filter: its points, the limit for Junk and the rules that counted (`X-Spam-Status`); `null` and `[]` when it did not look |
| `signals.inJunk` | the mail is in Junk now |
| `signals.sender` | the From address and this account's history with it: mails from it before this one, how many of them are in Junk, mails the person sent to it, whether it is in the address book, and when the first mail came (`UTCDate`) |

The model sees the same signals as facts next to the mail, and the mail itself
as untrusted data.

## Assist/extractEvents

Appointments, deadlines and trips in a mail, for the webmail's "add to
calendar".

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id` | or `foreignMails` ([Foreign mail](#foreign-mail)) |
| `includeImages` | `Boolean` | also read the text in the mail's pictures |

```json
["Assist/extractEvents", {
  "accountId": "a1",
  "emailId": "e42",
  "events": [{
    "title": "Dentist appointment",
    "start": "2026-10-06T09:30:00",
    "end": "2026-10-06T10:00:00",
    "allDay": false,
    "timeZone": "Europe/Berlin",
    "location": "Praxis Dr. Zahn, Hauptstraße 1",
    "description": null,
    "url": null,
    "participants": [{ "name": "Leni", "email": "leni@example.org" }],
    "confidence": 0.85,
    "quote": "Ihr Termin am Dienstag, 6. Oktober um 9:30 Uhr"
  }],
  "providerId": "q1", "providerName": "Mistral", "model": "mistral-small-latest",
  "usage": { "inputTokens": 1210, "outputTokens": 140, "reasoningTokens": 0 }
}, "0"]
```

- `start` and `end` are JMAP `LocalDateTime`s; all-day events start at
  `T00:00:00` and `end` is the day after the last one. An event whose `end`
  the mail does not give lasts an hour (a day when all-day).
- `timeZone` is an IANA name when the mail names or clearly implies one,
  otherwise `null` (the client uses the person's). `location`, `description`
  and `url` are `null` when the mail has none; `url` is only ever an `https`
  address that appears in the mail.
- `participants` are suggestions: people the mail names who are in the
  person's address book or in the mail's From, To or Cc, with the address from
  there. Names without an address found this way, and the person themselves,
  are left out.
- `quote` is the text the event was read from, as it stands in the mail (at
  most 300 characters); an event whose quote is not in the mail is dropped.
- At most 10 events; relative dates ("next Tuesday") are read from the day the
  mail was sent.

With `includeImages`, the text the server reads out of the mail's inline
pictures and attached images ([`Email/imageText`](jmap-image-text.md), when the
server can read pictures) is added to the mail's text. Remote pictures are not
read here. Pictures are never sent to the model itself.

The call does not depend on the person's `assist.refineEvents`: that setting
only decides whether the webmail calls it by itself when a mail opens. A
"find appointment" button calls it directly with the setting off.

## Labels

Labels are the person's own words for kinds of mail ("Rechnungen: invoices,
receipts, payment confirmations"). A label is a JMAP keyword on the email, so
every client sees it: IMAP apps show it as a tag or keyword. Labels never
move, delete or answer mail.

Labels work **without any AI provider**: managing them, filtering by them
(`Email/query` with `hasKeyword` and no `inMailbox` finds a label's mail in
every folder), and putting them on new mail by rules, detectors, learned
senders and the classifier. Only [`AssistLabel/suggest`](#assistlabelsuggest),
[`AssistLabel/apply`](#assistlabelapply) and the model's part of
[auto-labels](#auto-labels) need the `autoLabels` feature. The JMAP names
stay `AssistLabel/*`, as since 0.18.

| Method | Needs |
| --- | --- |
| `AssistLabel/get`, `/set`, `/log`, `/undo` | the capability (own account) |
| `AssistLabel/suggest`, `/apply` | the `autoLabels` feature (a provider), like any call to a model |

### AssistLabel

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | server-set, like `g3` |
| `name` | `String` | 1 to 40 characters, unique per person (ignoring case) |
| `description` | `String` | what belongs there, at most 300 characters; this is what the model reads |
| `keyword` | `String` | server-set when created and never changed: the keyword on the emails, a lower-case ASCII form of the first name (`rechnungen`, `bestellungen-versand`), `label-<form>` when that form is a mark other programs act on (`junk`, `nonjunk`, `notjunk`, `phishing`, `seen`, `answered`, `flagged`, `deleted`, `draft`, `recent`, `forwarded`, `mdnsent`, `submitpending`, `submitted`), or `label-<n>` |
| `color` | `String\|null` | `#rrggbb` or `null` |
| `rules` | `Rules\|null` | conditions that put the label on new mail; `null` for none (default) |
| `detector` | `String\|null` | a built-in detector that puts the label on new mail: `invoice`, `appointment`, `newsletter` or `shipping`; `null` for none (default) |
| `learnSenders` | `Boolean` | a sender whose mail the person gave this label by hand twice gets it on new mail (default `true`) |
| `classifier` | `Boolean` | the label's classifier may put it on new mail once it has learned enough (default `true`) |
| `totalEmails` | `Number` | server-set: emails of the account with the keyword, in any folder but those only in Junk or the Trash (the same as `Email/query` with `hasKeyword` and `inMailboxOtherThan` Junk and Trash) |
| `unreadEmails` | `Number` | server-set: of those, the ones without `$seen` |
| `examples` | `Number` | server-set: mails the classifier learned as having the label (given it by hand); it acts from 15 on |

`Rules`:

```json
{ "match": "any",
  "conditions": [
    { "field": "from", "value": "@stadtwerke.example" },
    { "field": "subject", "value": "Rechnung" },
    { "field": "hasAttachment", "value": "true" }
  ] }
```

| Field | Matches when |
| --- | --- |
| `from` | `value` with an `@` inside (`leni@example.org`): the From address is exactly that (ignoring case). Otherwise (`example.org`, `@example.org`): the From address's domain is that domain or a subdomain of it |
| `subject` | the subject contains `value` (ignoring case, runs of white space count as one space) |
| `text` | the mail's text (HTML turned into text) contains `value`, the same way |
| `hasAttachment` | `value` `"true"`: the mail has an attachment; `"false"`: it has none |

`match` is `all` (default) or `any`. At most `maxLabelConditions` (10)
conditions; `value` is 1 to 200 characters after trimming, without control
characters; `hasAttachment` takes only `"true"` or `"false"`. Rules with no
conditions are stored as `null`.

`AssistLabel/get` and `AssistLabel/set` are standard (`maxLabels` at most).
`rules`, `detector`, `learnSenders` and `classifier` may be set on create and
update; `totalEmails`, `unreadEmails` and `examples` are ignored in a patch when
they are unchanged and refused otherwise, like `id` and `keyword`. `SetError`
`invalidProperties` names the property (`rules`, `detector`, …) and says why.
Renaming a label keeps its keyword. Destroying one takes its keyword off every
email of the account and forgets its log, learned senders and classifier.

**State and push.** The `state` of `AssistLabel/get` (and `oldState`/`newState`
of `/set`) is its own and moves when a label changes, when the classifier
learned, and with every change to the account's mail, since the counts may
have moved (like `Mailbox`). `AssistLabel` is a push type: a `StateChange`
names it whenever its state moved, over EventSource, WebSocket and Web Push. A
client that shows counts fetches `AssistLabel/get` again then.

### Auto-labels

Every mail delivered to the person (after the spam filter, not into Junk, not
mail they sent themselves) gets labels in two steps:

1. **Without a model**, during delivery and **before the person's Sieve
   rules run** (with `nonAiLabels` on, the default): for each label, the
   first of these that matches puts it on:
   1. its `rules`,
   2. its `detector`,
   3. a learned sender (`learnSenders`): the From address got this label by
      hand at least twice, and never had it taken off by hand since,
   4. its classifier (`classifier`): a naive Bayes model of this person's
      mail, once it has at least 15 examples with and 15 without the label and
      is at least 99 % sure.

   How each of these decides is in [labels.md](labels.md); it is cheap, and
   delivery never waits long for it or fails because of it (after a second, or
   on any error, the mail is simply stored without these labels).
   The keywords go onto the stored mail directly. Sieve sees them: see
   [sieve.md](sieve.md#labels).
2. **With the model** (with `AssistSettings.autoLabels` on and the `autoLabels`
   feature): afterwards, in the background, the model judges only the labels
   **not yet** on the mail (set by step 1 or by Sieve), each in turn (a
   sentence why, then `fits` true or false, so the reason comes before the
   decision), and adds the keywords of those that fit. The model never takes a
   label off. Delivery never waits for it and never fails because of it: when
   the provider is away or the quota is used up, the mail simply keeps what it
   has (a job is tried three times over a few minutes, then dropped). Mail
   older than a day in the queue is dropped as well.

Every label put on this way is logged ([`AssistLabel/log`](#assistlabellog)).

**Learning from the person.** When the person puts a label's keyword on an
email or takes it off by hand — `Email/set`, IMAP `STORE`, or
`AssistLabel/undo` — the server learns from it (changes the server makes
itself, and keywords set by Sieve at delivery, teach nothing):

- putting it on counts the From address for the label; taking it off by hand
  forgets that address for the label entirely;
- the mail becomes an example for the label's classifier (with the label when
  put on, without it when taken off). For each mail given a label by hand, one
  recent unlabeled mail from the inbox is learned as an example without any
  label, so the classifier knows what ordinary mail looks like.

Learning happens in the background, a moment later; `examples` counts up then.

### AssistLabel/log

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailIds` | `Id[]\|null` | only these emails (at most 500); `null` for the latest |
| `limit` | `Number` | at most 500, default 100 |

Response `{ accountId, list: [{ id, emailId, labelId, name, keyword, source,
reason, code, params, providerName, model, createdAt, undone }] }`, newest
first.

| Field | |
| --- | --- |
| `source` | who put the label on: `ai`, `rule`, `sender`, `detector` or `classifier` (entries from before 0.21 are `ai`) |
| `reason` | one sentence why: the model's own words for `ai`; for the others an English sentence made from `code` and `params` (like `Looks like an invoice: PDF attachment "Rechnung_4711.pdf"`) |
| `code` | what the reason says, for a client to put in its own words: see below |
| `params` | `Object`, the details `code` names |
| `providerName`, `model` | who chose the label for `ai` (`null` when unknown); `null` for the others |
| `undone` | `true` once the person took the label off with `AssistLabel/undo` (or by removing the keyword) |

| `code` | `source` | `params` |
| --- | --- | --- |
| `ai` | `ai` | `{}` |
| `rule` | `rule` | `{ "match": "all"\|"any", "conditions": [{ "field", "value" }] }`: the conditions that matched |
| `sender` | `sender` | `{ "address": "leni@example.org", "count": 3 }`: how often the person gave this label to that sender's mail by hand |
| `invoice` | `detector` | `{ "attachment": "Rechnung_4711.pdf" }` or `{ "word": "Rechnung", "amount": "49,90 €" }` (`amount` may be `null`) |
| `appointment` | `detector` | `{ "calendar": true }` (an invitation or `.ics` in the mail) or `{ "word": "Terminbestätigung", "date": "06.10.2026", "time": "09:30" }` |
| `newsletter` | `detector` | `{ "header": "List-Id" }` or `{ "header": "Precedence" }` or `{ "header": "List-Unsubscribe-Post" }`: what gave it away besides `List-Unsubscribe` |
| `shipping` | `detector` | `{ "carrier": "DHL"\|"DPD"\|"Hermes"\|"UPS"\|"GLS"\|"Amazon"\|null, "tracking": "00340434161234567890"\|null }` |
| `classifier` | `classifier` | `{ "probability": 0.994, "examples": 23 }` |

New codes may come; a client that does not know one shows `reason`.

### AssistLabel/undo

`{ accountId, ids: [logId] }` takes the label off the email and marks the log
entry undone. Response `{ accountId, undone: [ids], notFound: [ids] }`. It
counts as taking the label off by hand: the sender is forgotten for the label
and the classifier learns the mail as one without it.

### AssistLabel/apply

`{ accountId, emailIds: [Id] }` (at most 20) asks the model now, for mail that
arrived before auto-labels were on or while it was off. It needs the
`autoLabels` feature, not the setting, judges only the labels not on the mail
yet, and logs what it puts on as `ai`. Response `{ accountId, labeled: {
emailId: [labelId] }, notFound: [ids] }`.

### AssistLabel/suggest

"Label again": the model judges **every** label for one mail (also those on
it, so the person can take off ones that no longer fit), and when none fits,
proposes up to two new labels. It changes nothing: the client applies what the
person ticks with `Email/set` (`keywords/<keyword>`), which counts as by hand,
and creates a proposed label with `AssistLabel/set` first. It needs the
`autoLabels` feature (not the setting) and counts as a request of it.

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id` | the mail (or `foreignMails`, see [Foreign mail](#foreign-mail)) |
| `suggestNew` | `Boolean` | may propose new labels (default `true`); `false` asks only for the verdicts, which is shorter |
| `language` | `String\|null` | the language of reasons and new labels (`de`); the language of the label descriptions otherwise |

```json
["AssistLabel/suggest", {
  "accountId": "a1",
  "emailId": "e42",
  "verdicts": [
    { "labelId": "g3", "name": "Rechnungen", "reason": "Die Mail ist eine Rechnung der Stadtwerke über 49,90 €.",
      "fits": true, "isSet": false },
    { "labelId": "g5", "name": "Reisen", "reason": "Es geht nicht um eine Reise.", "fits": false, "isSet": true }
  ],
  "newLabels": [],
  "providerId": "q1", "providerName": "Mistral", "model": "mistral-small-latest",
  "usage": { "inputTokens": 912, "outputTokens": 120, "reasoningTokens": 0 }
}, "0"]
```

- `verdicts`: one per label, in the order of the person's labels; `reason`
  (one sentence, at most 300 characters) is written before `fits`. A label the
  model did not answer for is left out. `isSet`: the keyword is on the mail
  now.
- `newLabels`: `[{ name, description, color, reason }]`, 0 to 2, only when no
  label fits (otherwise always `[]`), and never more than `maxLabels` leaves
  room for. `name` is 1 to 40 characters and not the name of an existing label
  (ignoring case), `description` at most 300 characters, `color` `#rrggbb`
  (the server picks one when the model gave none that is valid), `reason` one
  sentence. Proposals that fail these are dropped.
- The answer is JSON of a fixed shape the provider is held to where it can
  be; what does not fit the shape is dropped as above.

Errors as for the other calls; `notFound` for an email that is not there.
Without any label, `verdicts` is `[]` and the model is still asked for
`newLabels` (when `suggestNew`).

## Assist/estimate

What one of the calls above would take, for a hint like "≈ 1,200 tokens ·
48,000 left today" on a button. The server builds the same prompt the call
would build (the same checks of the arguments, the same cutting of the mail
to size, the same mails of a conversation, the same provider and model), but
asks no provider and counts nothing: an estimate is not a request and does not
use up any of the day's limits.

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `method` | `String` | `Assist/compose`, `Assist/summarize`, `Assist/spamCheck`, `Assist/extractEvents` or `AssistLabel/suggest` |
| `arguments` | `Object` | exactly what that method would get, `foreignMails` and `foreignLabels` included; its `accountId` may be left out |
| `currency` | `String` | ISO 4217 code of `cost`, default `EUR` |

```json
["Assist/estimate", {
  "accountId": "a1",
  "method": "Assist/summarize",
  "arguments": { "threadId": "t7" }
}, "0"]
```

```json
["Assist/estimate", {
  "accountId": "a1",
  "method": "Assist/summarize",
  "inputTokens": 1189,
  "outputTokens": 250,
  "reasoningTokens": 500,
  "totalTokens": 1939,
  "imageCount": 0,
  "imageTokens": 0,
  "calls": [
    { "purpose": "main", "inputTokens": 1189, "outputTokens": 250, "reasoningTokens": 500,
      "images": 0, "weight": 1 }
  ],
  "calibrated": false,
  "providerId": "q1", "providerName": "OpenAI", "model": "gpt-5-mini",
  "tokensLeftToday": 48000,
  "requestsLeftToday": null,
  "cost": {
    "amount": 0.00148, "currency": "EUR", "usd": 0.00173,
    "max": { "amount": 0.00710, "usd": 0.00830 },
    "parts": { "input": 0.00025, "output": 0.00043, "reasoning": 0.00085,
               "images": 0, "requests": 0, "other": 0 }
  }
}, "0"]
```

Everything one request can take is counted: every call to the model it makes,
the prompt with what the API adds around it, the answer and the model's
thinking.

- `calls` are the calls to the model the request makes or may make, each with
  its tokens and a `weight`, how likely it is:
  - `main`, weight 1: the request itself.
  - `retry`: a provider that refuses the answer's JSON shape (HTTP 400; spam
    check and events only) is asked once more without it. Listed only when
    that happened in this provider's and model's last requests for the
    feature, with the share of them it happened in as `weight`, and with the
    prompt's tokens (the big providers don't bill a refused request, some
    servers do; counted to be on the safe side).

  Nothing else calls a model: the text in pictures is read by the server
  itself (Tesseract, [jmap-image-text.md](jmap-image-text.md)), a
  conversation is summarized in one call with each mail cut to its share, and
  there are no second passes. `images` (pictures sent to the model as
  pictures) is therefore always `0`.
- `inputTokens`, `outputTokens` and `reasoningTokens` are the sums over
  `calls`, each by its `weight`; `totalTokens` is the sum of the three.
- `inputTokens` of a call is the prompt (instructions, the mail's text, the
  text of pictures with `includeImages` and, for spam check and events, the
  JSON shape of the answer), counted the way the server counts a request before
  it is sent: about four characters to a token, a token for each Chinese,
  Japanese or Korean character; plus what the API adds (the roles and markers
  of its messages, about 10 tokens, and 12 more for a JSON shape).
- `outputTokens` is a **typical** answer, not the most the model may write
  (which is far more than it usually does): `compose` 400 for `write`, for
  `rewrite` and `adjust` about the draft's length (a quarter more, at least
  100); `summarize` 150 for one mail, 50 more per further mail of a
  conversation, at most 600; `spamCheck` 150; `extractEvents` 250;
  `AssistLabel/suggest` 40 per label, and 120 more with `suggestNew`. Never more
  than the call allows the model (nor the model's own `maxOutputTokens`).
- `reasoningTokens` is typical thinking of a model that thinks before it
  answers: by the price lists' `supportsReasoning` or, where they don't know
  the model, by its name (`o3`, `gpt-5…`, `gemini-2.5-…`, `deepseek-r1`,
  `qwen3`, …). `compose` 700, `summarize` 500, `spamCheck` 500,
  `extractEvents` 900, `AssistLabel/suggest` 300; `0` for a model that does not
  think and for Anthropic's
  models (they think only when asked to, which the server does not do). Thinking
  and answer together stay within what the call allows the model.
- `calibrated`: `true` once there are at least 5 real requests of this
  provider's model for the feature. The server keeps, for the last 50 of them,
  what it expected (prompt and typical answer) and what the provider reported,
  and from then on takes the prompt times the median of real to expected
  prompt tokens, the typical answer times the median of real to typical answer
  (each ratio held to 0.5 to 3), the median of the real thinking, and the share
  of requests asked twice for `retry`. Requests whose tokens the provider did
  not report are not counted.
- `imageCount` is the number of pictures whose text goes along with
  `includeImages` (read before, or about 100 tokens for one never read), and
  `imageTokens` the tokens of that text, part of `inputTokens`.
- `cost` is what all this costs at the model's price (see `price` of
  `AssistProvider`), in `currency` by the ECB's reference rates of the day
  (rough built-in rates for `USD`, `JPY` and `CNY` until the server got them
  once), and in US dollars (`usd`), the currency of the price lists:
  - thinking at `reasoningPerMillion`, a request's `perRequest` fee for each
    call, the higher price of a tier when the prompt is above it; the prompt at
    the full price (only Anthropic's explicit cache is predictable, and the
    server does not use it; what a provider did read from its cache lowers the
    real cost afterwards, see `Assist/usage`);
  - `max` (`amount` in `currency`, `usd`) is the worst case: the prompt at
    least as the heuristic counts it, the whole answer allowance
    (`maxOutputTokens` of the model or of the call, whichever is lower) spent
    at the dearer of answer and thinking, and for spam check and events one
    retry;
  - `parts` splits `amount`, in `currency`: `input` (the prompt without the
    pictures' text), `output`, `reasoning`, `images` (the pictures' text),
    `requests` (per-request fees), `other` (the extra calls of `calls`). They
    add up to `amount`.

  `cost` is `null` when the price is not known, when there is no rate for
  `currency`, or when the admin does not show this server provider's costs. A
  free provider (Ollama, a ChatGPT subscription) answers `0`.
- `tokensLeftToday` and `requestsLeftToday` are what is left of the person's
  daily limits of that provider, `0` when used up (the call itself would then
  answer `overQuota`); `null` when that limit does not exist, and always for
  the person's own providers.
- With `includeImages`, pictures are **not** read for an estimate: the text of
  pictures that were read before (by `Email/imageText` or an earlier call) is
  taken from the server's cache, each picture never read counts as about 100
  tokens.
- Errors are those of the call: `assistUnavailable` when the feature is off or
  no provider can be used, `notFound`, `invalidArguments` for arguments the
  call would refuse, and `invalidArguments` for another `method`. At most four
  estimates run at once per person; more answer `providerFailed` with
  `retryAfter`. An estimate reads the mail but nothing else is slow: it is
  meant to be asked when a pointer rests on a button, and a client keeps the
  answer until the mail or the draft changes.

## Foreign mail

The UwUMail app can use this server's assistant for the mail of its **other**
accounts (Exchange, Gmail, any IMAP), when the admin allows it: the account
capability's `foreignMail` is `true`. The app then sends the mail's content
itself, since the server does not have that mail.

The admin allows it with the switch "AI for mail from other accounts" (off by
default) for the whole server, and per server provider (a provider's features
include `foreignMail`); a person's own providers may be used for it whenever the
switch is on. A foreign request uses the provider and model the person chose
for the feature when that provider allows foreign mail, and the first one that
does otherwise ([AssistSettings](#assistsettings)). It counts against the same
daily limits as any request of the feature, and it is logged in
[`Assist/usage`](#assistusage) like one. **Nothing else of it is kept**: not
the mail, not the labels, not the answer.

These calls take `foreignMails` **instead of** their mail ids (giving both is
`invalidArguments`; without `foreignMail` it is `assistUnavailable`):

| Call | Instead of | `foreignMails` |
| --- | --- | --- |
| `Assist/summarize` | `emailId` / `threadId` | 1 to 20 mails, oldest first (a conversation); the answer has `emailId` and `threadId` `null` |
| `Assist/spamCheck` | `emailId` | exactly 1 |
| `Assist/extractEvents` | `emailId` | exactly 1; `includeImages` must be `false` or left out |
| `Assist/compose` | `replyToEmailId` | exactly 1: the mail being answered |
| `AssistLabel/suggest` | `emailId` | exactly 1, with `foreignLabels` |
| `Assist/estimate` | | the same `arguments` as the call |

A foreign mail:

```json
{
  "from": [{ "name": "Stadtwerke", "email": "rechnung@stadtwerke.example" }],
  "to": [{ "name": null, "email": "leni@example.org" }],
  "cc": [],
  "date": "2026-09-30T08:12:00Z",
  "subject": "Ihre Rechnung September",
  "text": "Guten Tag, anbei Ihre Rechnung …",
  "headers": [{ "name": "Authentication-Results", "value": "mx.example.net; spf=pass …" }],
  "inJunk": false
}
```

| Field | Type | |
| --- | --- | --- |
| `from`, `to`, `cc` | `EmailAddress[]` | as in `Email/get`, at most 50 each; `name` at most 200 characters, `email` at most 320 |
| `date` | `UTCDate\|null` | when it was sent; relative dates in the mail are read from it (now when `null`) |
| `subject` | `String` | at most 998 characters |
| `text` | `String` | the body as plain text (the client turns HTML into text), at most 200,000 characters. The server removes the quoted history and cuts it to size exactly as it does its own mail |
| `headers` | `{name, value}[]\|null` | optional, `Assist/spamCheck` only (ignored elsewhere): at most 100, `name` at most 100 characters, `value` at most 2,000 |
| `inJunk` | `Boolean` | optional, `Assist/spamCheck` only: the mail is in the account's junk folder |

Anything larger is `invalidArguments` naming the field; nothing is cut
silently but the text.

The mail goes to the model exactly like the person's own mail: as data between
tags, after the same rules that it is not to be obeyed, with the same cutting
to size.

**Spam check.** `signals` are what the server can tell from the given
headers: `authentication` from the topmost `Authentication-Results` of
`headers`, whichever server wrote it (for own mail only this server's own
counts); `spamScore`, `spamThreshold` and `tests` from `X-Spam-Status` if
there is one; `inJunk` from the mail. `sender` is `null`: the server knows
nothing about the history of another account. The model is told that these
results come from the other provider.

**Labels.** `AssistLabel/suggest` takes, besides one foreign mail,
`foreignLabels`: the labels of that account as the app keeps them, at most 50:

```json
"foreignLabels": [
  { "name": "Rechnungen", "description": "Rechnungen, Quittungen", "isSet": false },
  { "name": "Reisen", "description": "", "isSet": true }
]
```

`name` 1 to 40 characters, unique ignoring case; `description` at most 300;
`isSet` whether the label is on the mail now (default `false`). Verdicts then
name labels by `name`, and `labelId` is `null`:

```json
{ "labelId": null, "name": "Rechnungen", "reason": "…", "fits": true, "isSet": false }
```

`newLabels` are checked against `foreignLabels` names, and `maxLabels` does not
apply. `emailId` in the answer is `null`.

**Auto-labels of foreign mail.** The app puts labels on new foreign mail
without a model itself, with the same rules, detectors, sender learning and
classifier as the server ([labels.md](labels.md)). For the labels that are still
not set it may then ask the server's model: `AssistLabel/suggest` with
`suggestNew: false` and `foreignLabels` holding only those labels, and it sets
the ones with `fits: true`. It must never take a label off because of such an
answer.

## Assist/usage

`{ accountId, days, currency }` (1 to 90, default 30; `currency` as in
`Assist/estimate`, default `EUR`) answers what this person used:

```json
["Assist/usage", {
  "accountId": "a1",
  "days": [
    { "day": "2026-09-29", "providerId": "q1", "providerName": "Mistral", "feature": "summarize",
      "requests": 4, "inputTokens": 5210, "outputTokens": 380, "reasoningTokens": 1600,
      "cachedTokens": 1024, "calls": 4,
      "cost": { "amount": 0.00061, "currency": "EUR", "usd": 0.00071 } }
  ],
  "today": [
    { "providerId": "q1", "providerName": "Mistral", "requests": 9, "tokens": 12020,
      "requestsPerDay": 200, "tokensPerDay": null, "cost": null }
  ]
}, "0"]
```

Days are UTC. Tokens are what the provider reported; where a provider reports
none (some OpenAI-compatible servers while streaming), the server estimates
about four characters per token. `outputTokens` is the answer's text,
`reasoningTokens` what the model spent thinking on top of it (OpenAI's
`completion_tokens_details.reasoning_tokens`, Gemini's thoughts, the Responses
API's `output_tokens_details.reasoning_tokens`; Anthropic counts thinking in
`outputTokens`), `cachedTokens` the part of `inputTokens` the provider read
from its cache, `calls` the calls to the model (a request asked again after
its JSON shape was refused counts two). `tokens` of `today` is input, output
and thinking together, like the daily limit counts them. `cost` is kept with
each request, in US dollars at the price of the time: what OpenRouter says it
charged (`usage.cost`) when it says so, otherwise the reported tokens at the
model's price, cached prompt tokens at `cacheReadPerMillion`, thinking at
`reasoningPerMillion`, the tier the prompt reached, and the per-request fee.
It is shown in `currency` at today's rate;
`null` where the price was not known (and for everything before 0.19.0), and
for server providers whose costs the admin does not show.

## Streaming

`Assist/compose` and `Assist/summarize` can also be called at `streamUrl`, so
the text appears while the model writes it:

```
POST /jmap/assist/stream
Content-Type: application/json

{ "using": ["urn:ietf:params:jmap:core", "urn:uwumail:jmap:assist"],
  "method": "Assist/compose",
  "arguments": { "accountId": "a1", "mode": "write", "instruction": "Sag Mia für Freitag zu" } }
```

It takes the same login as `apiUrl` (the portal's session cookie with the
CSRF header, Basic, or a bearer token). A request that is not authenticated
gets `401`, one that is not JSON or names another method `400`. Everything
else answers `200` with `Content-Type: text/event-stream` and these events:

| Event | `data` |
| --- | --- |
| `subject` | `{ "subject": "…" }`: `write` with `wantSubject`, before the text |
| `delta` | `{ "text": "…" }`: the next piece of text |
| `done` | the method's whole response, as `Assist/compose` or `Assist/summarize` would answer it |
| `error` | `{ "type": "…", "description": "…" }`, the method error; the stream ends |

A comment line (`: ping`) is sent every 15 seconds while the model thinks.
Closing the connection stops the request to the provider.

## Privacy and limits

- Labels without a model (rules, detectors, senders, the classifier) never
  send anything anywhere.
- Only what the feature needs is sent: the mail's text (HTML turned into
  text), without quoted history below "On … wrote:" and `>` lines, cut to
  `maxTextChars`; subject, date, and names and addresses of sender and
  recipients. Attachments are not sent. For labels, only the labels' names and
  descriptions and the start of the mail.
- A request to a provider times out after 60 seconds without a byte of answer
  and 3 minutes in total; answers larger than 1 MB are refused.
- Every request counts against the daily quota of the provider it went to, per
  person, in requests and tokens; the admin sees the counts per person and day.
