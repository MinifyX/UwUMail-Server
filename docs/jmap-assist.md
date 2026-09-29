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
  "maxInstructionChars": 2000,
  "maxTextChars": 20000
}
```

| Property | |
| --- | --- |
| `features` | per feature, whether this person can use it right now: the admin allows it and some provider the person may use is allowed it. `autoLabels: true` means it *can* be switched on; whether it is on is in [`AssistSettings`](#assistsettings). |
| `mayAddProviders` | the admin lets people add providers with their own keys |
| `mayUsePrivateAddresses` | such a provider may point into the local network (Ollama on the LAN) |
| `maxProviders` | how many providers of their own one person may have |
| `maxLabels` | how many labels one person may have |
| `maxInstructionChars` | longest `instruction` of `Assist/compose` |
| `maxTextChars` | longest `text` of `Assist/compose`; mail content is cut to about this much too |

The session `state` changes when `features` or the two `may…` flags change, so
a client fetches the session again and sees the new capability.

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

### AssistProvider/get

Standard `/get`. `ids: null` answers every provider the person may use: the
server's that are allowed for them (in the admin's order) and their own. The
`state` changes when any of them changes.

### AssistProvider/set

Standard `/set` for the person's **own** providers (`create`, `update`,
`destroy`); server providers answer `forbidden`. Creating needs
`mayAddProviders`. Properties that may be set: `name`, `kind` (create only),
`baseUrl`, `apiKey`, `model`, `fastModel`.

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
| `autoLabels` | `Boolean` | labels are put on incoming mail (opt-in, `false` by default) |
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
| `wantSubject` | `Boolean` | `write` only: also propose a subject (default `false`) |
| `language` | `String\|null` | the person's UI language as a hint (`de`); the model otherwise answers in the language of the instruction or the mail |

Response: `{ accountId, text, subject, providerId, providerName, model, usage }`
with `subject` a proposal or `null`, and `usage` `{ inputTokens, outputTokens }`.

## Assist/summarize

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailId` | `Id\|null` | one mail … |
| `threadId` | `Id\|null` | … or a whole conversation (at most its latest 20 mails, oldest first) |
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
| `emailId` | `Id` | |
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
  "usage": { "inputTokens": 1830, "outputTokens": 96 }
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
| `emailId` | `Id` | |
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
  "usage": { "inputTokens": 1210, "outputTokens": 140 }
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
pictures and attached images ([`Email/imageText`](jmap-imagetext.md), when the
server can read pictures) is added to the mail's text. Remote pictures are not
read here. Pictures are never sent to the model itself.

## Labels

Labels are the person's own words for kinds of mail ("Rechnungen: invoices,
receipts, payment confirmations"). A label is a JMAP keyword on the email, so
every client sees it: IMAP apps show it as a tag or keyword. The model only
ever picks among the person's labels; it never moves, deletes or answers mail.

### AssistLabel

| Property | Type | |
| --- | --- | --- |
| `id` | `Id` | server-set, like `g3` |
| `name` | `String` | 1 to 40 characters, unique per person (ignoring case) |
| `description` | `String` | what belongs there, at most 300 characters; this is what the model reads |
| `keyword` | `String` | server-set when created and never changed: the keyword on the emails, a lower-case ASCII form of the first name (`rechnungen`, `bestellungen-versand`), or `label-<n>` |
| `color` | `String\|null` | `#rrggbb` or `null` |

`AssistLabel/get` and `AssistLabel/set` are standard (`maxLabels` at most).
Renaming a label keeps its keyword. Destroying one takes its keyword off every
email of the account and forgets its log.

### Auto-labels

With `AssistSettings.autoLabels` on and at least one label, every mail that is
delivered to the person (after the spam filter, not into Junk, not for mail they
sent themselves) is queued. A background worker asks the model which of the
labels fit (none, one or several) and why, then sets the keywords. Delivery
never waits for it and never fails because of it: when the provider is away or
the quota is used up, the mail simply stays without labels (a job is tried
three times over a few minutes, then dropped). Mail older than a day in the
queue is dropped as well.

### AssistLabel/log

| Argument | Type | |
| --- | --- | --- |
| `accountId` | `Id` | |
| `emailIds` | `Id[]\|null` | only these emails (at most 500); `null` for the latest |
| `limit` | `Number` | at most 500, default 100 |

Response `{ accountId, list: [{ id, emailId, labelId, name, keyword, reason,
providerName, model, createdAt, undone }] }`, newest first. `reason` is the
model's one sentence why; `providerName` and `model` say who chose the label
(`null` when unknown); `undone` is `true` once the person took the label off with
`AssistLabel/undo` (or by removing the keyword).

### AssistLabel/undo

`{ accountId, ids: [logId] }` takes the label off the email and marks the log
entry undone. Response `{ accountId, undone: [ids], notFound: [ids] }`.

### AssistLabel/apply

`{ accountId, emailIds: [Id] }` (at most 20) asks the model now, for mail that
arrived before auto-labels were on or while it was off. It needs the
`autoLabels` feature, not the setting. Response `{ accountId, labeled: {
emailId: [labelId] }, notFound: [ids] }`.

## Assist/usage

`{ accountId, days }` (1 to 90, default 30) answers what this person used:

```json
["Assist/usage", {
  "accountId": "a1",
  "days": [
    { "day": "2026-09-29", "providerId": "q1", "providerName": "Mistral", "feature": "summarize",
      "requests": 4, "inputTokens": 5210, "outputTokens": 380 }
  ],
  "today": [
    { "providerId": "q1", "providerName": "Mistral", "requests": 9, "tokens": 12020,
      "requestsPerDay": 200, "tokensPerDay": null }
  ]
}, "0"]
```

Days are UTC. Tokens are what the provider reported; where a provider reports
none (some OpenAI-compatible servers while streaming), the server estimates
about four characters per token.

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

- Only what the feature needs is sent: the mail's text (HTML turned into
  text), without quoted history below "On … wrote:" and `>` lines, cut to
  `maxTextChars`; subject, date, and names and addresses of sender and
  recipients. Attachments are not sent. For labels, only the labels' names and
  descriptions and the start of the mail.
- A request to a provider times out after 60 seconds without a byte of answer
  and 3 minutes in total; answers larger than 1 MB are refused.
- Every request counts against the daily quota of the provider it went to, per
  person, in requests and tokens; the admin sees the counts per person and day.
