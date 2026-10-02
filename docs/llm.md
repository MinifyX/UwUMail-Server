# AI assistant

UwUMail can ask a language model for help with mail: writing and rewriting
drafts, summing up a mail or a whole conversation, a second opinion on spam,
finding dates for the calendar, and putting the person's own labels on new mail.
Nothing of this is on until the admin sets up a provider (or lets people bring
their own), and every request goes out from the server, never from the browser
or the app.

The protocol for clients is in [jmap-assist.md](jmap-assist.md); this page is
for admins and for people setting it up for themselves.

## What it does

| Feature | Where | Starts |
| --- | --- | --- |
| Write and rewrite | Composer: "Schreiben lassen", the presets (more formal, more casual, shorter, friendlier, clearer, spelling, translate) and "Anpassen…" with an own instruction | a click; the draft only changes when the person inserts or replaces |
| Summaries | Reader: one mail or the whole conversation | a click |
| Spam check | Reader: "Auf Spam prüfen" | a click; shows the model's verdict next to what the server itself knows, and offers "Spam" / "Kein Spam" |
| Dates for the calendar | Reader, with the dates the server finds itself | a click ("find appointment", whatever the setting), or on opening a mail when the person switched on `assist.refineEvents` (off by default) |
| Auto-labels | New mail in the inbox | on delivery, only for people who switched it on |
| Label again | Reader: the model judges every label for one mail and proposes new ones when none fits (`AssistLabel/suggest`) | a click |
| Mail of other accounts | The UwUMail app, for its Exchange, Gmail and IMAP accounts | only when the admin allows it ([below](#mail-of-other-accounts)) |

The features started by a click are available as soon as a provider is: the
person does not have to switch them on. Auto-labels are **opt-in** per person.

Every answer says which provider and model gave it, and the settings show
per feature what will be used.

**Labels themselves need no AI.** A person's labels, their rules, the built-in
detectors (invoices, appointments, newsletters, shipping), learned senders and
each label's classifier work on every server with the assistant, also without
any provider ([labels.md](labels.md), [jmap-assist.md](jmap-assist.md#labels));
only the model's part of auto-labels and "Label again" need one.

## Setting it up (admin)

**In the portal:** *Server → Settings → AI assistant*.

1. **Add a provider**: pick its kind, paste the key, and press *Load models* to
   check the key and choose models. Each provider has two: the *model* for
   writing and the cheaper, faster *fast model* for everything else
   (summaries, spam checks, dates, labels). The kinds suggest sensible, cheap
   defaults (below).
2. **Who may use it**: everyone, the people of some domains, or some people.
3. **What for**: each provider can be limited to some features.
4. **Daily limits** per person: requests and/or tokens a day (UTC). Without a
   limit a person can use it as much as they like; the admin sees the counts
   under *Usage* either way. A request counts when it starts, before the
   provider is asked, and before any mail is read for it; its tokens are added
   when it ends, estimated from what was sent and received when it failed or
   the reader left a streamed answer. `Assist/estimate` (the token hint on
   the AI buttons) counts nothing and asks no provider; it shows what is left
   of these limits.
5. **The policy** for the whole server:
   - which features exist at all (switched off here, a feature is gone for
     everyone, including own providers);
   - whether people may add **providers of their own** with their own keys
     (off by default), and
   - whether those may point **into the local network** (an Ollama at home;
     off by default, see [Addresses](#addresses-and-ssrf));
   - **AI for mail from other accounts** (off by default, see
     [below](#mail-of-other-accounts)).

Changes are in the change log, without the key.

## Setting it up (person)

Under *My account → AI assistant* in the portal, or *Settings → KI-Assistent*
in the webmail:

- choose the default provider and model, and, if wanted, another one per
  feature;
- add own providers, when the admin allows it;
- switch auto-labels on and manage the labels;
- see today's use against the limits, and the last 30 days.

## Providers

| Kind | Key | Address | Model / fast model (defaults) |
| --- | --- | --- | --- |
| OpenAI | API key | `https://api.openai.com/v1`, may be changed (a gateway) | `gpt-5-mini` / `gpt-5-nano` |
| Anthropic Claude | API key | `https://api.anthropic.com/v1`, may be changed (a gateway) | `claude-sonnet-5` / `claude-haiku-4-5` |
| Google Gemini | AI Studio key | fixed | `gemini-2.5-flash` / `gemini-2.5-flash-lite` |
| Mistral | API key | fixed | `mistral-medium-latest` / `mistral-small-latest` |
| OpenRouter | API key | fixed | `openai/gpt-5-mini` / `google/gemini-2.5-flash-lite` |
| Ollama | none | required | none, pick one from the list |
| OpenAI-compatible | optional | required | none, pick one from the list |
| ChatGPT (experimental) | sign-in | fixed | `gpt-5.1` / `gpt-5.1-codex-mini` |

Two API shapes are spoken: OpenAI's **Chat Completions** (OpenAI, Gemini,
Mistral, OpenRouter, Ollama and every compatible server) and Anthropic's
**Messages**. Answers that must have a shape (spam verdicts, dates, labels) are
asked for as JSON with a schema; a server that does not know schemas is asked
once more without one, and the answer is checked either way.

### OpenAI

Create a key at <https://platform.openai.com/api-keys> (a project key with
access to the models you want is enough). A ChatGPT Plus or Pro subscription is
not an API key; see [ChatGPT](#chatgpt-experimental) for that.

### Anthropic Claude

Create a key in the Claude Console at
<https://console.anthropic.com/settings/keys>. **Only API keys work.** A Claude
Pro or Max subscription cannot be used: Anthropic does not allow its
subscriptions (their sign-in and tokens) to be used by other programs such as
this one, so UwUMail offers no "sign in with Claude" and never will unless
Anthropic allows it. API use is billed per token in the Console.

### Google Gemini

Create a key in Google AI Studio at <https://aistudio.google.com/apikey>.
UwUMail uses Gemini's OpenAI-compatible endpoint. Mind the terms of the free
tier: Google may use what is sent on it to improve its products, and people may
read it. For mail, use a key of a project with billing switched on.

### Mistral

Create a key at <https://console.mistral.ai/api-keys>. Mistral runs in the EU.

### OpenRouter

Create a key at <https://openrouter.ai/settings/keys>. OpenRouter passes
requests on to many providers; model names carry the provider
(`anthropic/…`, `openai/…`). What happens to the data depends on where it is
routed: OpenRouter's privacy settings let you exclude providers that train on
or keep prompts.

### Ollama

For models on your own machine: `http://<address>:11434` (UwUMail adds `/v1`).
No key. Ollama listens on `127.0.0.1` only by default; for the mail server to
reach it, start it with `OLLAMA_HOST=0.0.0.0` (on a machine only your network
reaches). From the Docker container, the machine the server runs on is not
`localhost`: use its address in the network, or run Ollama as another service
in `compose.yaml` and use `http://ollama:11434`.

Small local models answer slowly and follow the schemas less reliably; the
server checks every answer and drops what does not fit.

### OpenAI-compatible

Anything that speaks Chat Completions: LM Studio, vLLM, llama.cpp's server,
LiteLLM, LocalAI, a company gateway. Give the address up to and including the
version (`https://llm.example.com/v1`) and a key if it needs one.

### ChatGPT (experimental)

**Experimental, and at your own risk.** A person can sign in with their
ChatGPT subscription the way OpenAI's Codex CLI does (`codex login
--device-auth`): the portal or webmail shows a code, the person confirms it at
OpenAI, and the server keeps the tokens (sealed) and renews them before they
run out. Requests then go to the backend Codex uses
(`https://chatgpt.com/backend-api/codex/responses`) with the ChatGPT account in
a header.

- This is **not an API OpenAI offers to other programs**. It may change or stop
  working without notice, and using a subscription from another program may be
  against OpenAI's terms. The portal and webmail say so before signing in.
- It is only offered as a provider of a person's own, so only with *own
  providers* allowed, and only for that person.
- What was taken from the Codex source (codex-rs `login` and `core`): the
  device-code endpoints (`/api/accounts/deviceauth/usercode`, `…/token`), the
  code exchange at `/oauth/token` with the Codex client id and the redirect
  `https://auth.openai.com/deviceauth/callback`, the refresh, the account id
  from the ID token, and the request headers (`chatgpt-account-id`,
  `originator: codex_cli_rs`) and body (`store: false`, streamed).
- **Not verified**, because it could not be tried against the real service:
  - which models a subscription may use: Codex reads that list at run time;
    UwUMail offers `gpt-5.1`, `gpt-5.1-codex`, `gpt-5.1-codex-max` and
    `gpt-5.1-codex-mini` and lets the person type another name;
  - whether the backend accepts instructions other than Codex's own (it may
    insist on them) and JSON schemas in `text.format`;
  - whether it wants a client `version` header, which Codex sends and UwUMail
    does not.

If signing in works and requests fail, the error the backend gave is shown;
please report it.

## Addresses and SSRF

A provider's address decides where the server connects, so it is checked
twice: when it is saved, and again for every address a name resolves to when
connecting.

| Provider set by | May reach |
| --- | --- |
| the admin | anywhere, including the local network and the machine itself (an Ollama next to the server) |
| a person, by default | the internet only, over `https://` |
| a person, with *own providers in the local network* allowed | also private networks (10/8, 172.16/12, 192.168/16, 100.64/10, fc00::/7), where plain `http://` is allowed too |

Never for a person's provider: the server itself (`127.0.0.0/8`, `::1`,
`localhost`), link-local addresses (cloud metadata at `169.254.169.254`) and
other special ranges. A name that resolves there later is refused when
connecting, not only when saved. Addresses may not carry a login, a query or a
fragment; plain `http://` never goes to the internet.

Redirects are not followed.

## The way out: `egress.assist`

With a VPN or proxy set up ([configuration.md](configuration.md#remote-pictures-through-a-vpn)),
`egress.assist` sends the requests to providers on the internet through it, so
OpenAI and the others see the VPN instead of the server. Off by default: the
providers know the account anyway through the key, and some block VPN
addresses. Providers in the local network are always reached directly.

```toml
[egress]
assist = false
```

As an environment variable `UWUMAIL_EGRESS__ASSIST`; in the portal under
*Server → Settings → VPN & proxy*.

## Costs

The server knows what most models cost and shows it: on the AI buttons with
the token estimate, in a person's usage and in the admin's statistics.

- **Price lists**, fetched once a day through the egress like the requests to
  providers (`egress.assist`), kept in the database; a list that can't be
  fetched keeps its last good copy:
  - [LiteLLM's price list](https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json)
    for most providers, US dollars: `input_cost_per_token`,
    `output_cost_per_token`, `output_cost_per_reasoning_token`,
    `cache_read_input_token_cost`, `cache_creation_input_token_cost`,
    `input_cost_per_image`, `input_cost_per_request` (or `_per_query`),
    `search_context_cost_per_query`, the higher prices of large prompts
    (`*_above_128k_tokens`, `*_above_200k_tokens`), and `supports_reasoning` and
    `max_output_tokens` about the model; a model is found with or without a
    provider prefix (`mistral/…`) and a date at its end (`-2025-08-07`,
    `-20251001`, `-latest`);
  - OpenRouter's own prices from its `/api/v1/models` (`prompt`, `completion`,
    `request`, `image`, `internal_reasoning`, `input_cache_read`,
    `input_cache_write`, `web_search`), while someone uses an OpenRouter
    provider;
  - the [ECB's euro reference rates](https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml)
    to show costs in euros, yen, yuan and the other currencies it lists. Until
    the server got them once (no way out to the internet yet), rough built-in
    rates for US dollars, yen and yuan stand in, so a price set by hand shows
    in euros all the same.
- **Free**: Ollama and a ChatGPT subscription cost nothing per request.
- **Set by hand**: the admin (for server providers) and a person (for their
  own) can set a price in US dollars per million tokens, in and out, and per
  request. It comes before the lists; what is not set comes from the lists.
  Thinking then costs what the answer costs, the cache what the prompt costs,
  and the lists' higher prices for large prompts no longer apply. Needed for
  models the lists don't know (an OpenAI-compatible server, a new model).
- **Who sees it**: a person always sees what their own providers cost. What a
  server provider costs they see only when the admin switched on *Show costs
  to users* for it (off by default). The admin always sees all costs.
- **Currency**: by the language of the portal, webmail or app: Japanese in yen,
  Chinese in yuan, all others in euros; in English a person may choose US
  dollars instead (the user setting `assist.currency`).
- **What is kept**: every request's cost is kept with the usage, in US
  dollars at the price of the moment; costs before 0.19.0 are not known. It is
  what the provider reported: the prompt (the part read from the provider's
  cache at the cache price), the answer, the model's thinking, the per-request
  fee, and the higher price of a large prompt; OpenRouter's own `usage.cost`
  where it gives one.
- **Estimates** (`Assist/estimate`, [jmap-assist.md](jmap-assist.md#assistestimate))
  count every call a request makes, the API's framing, a typical answer and
  the typical thinking of models that think, with a worst case. They learn
  from the last 50 real requests per provider, model and feature (tokens only,
  no content). They stay estimates: providers count with their own
  tokenizers.

## Keys

- Keys (and ChatGPT's tokens) are sealed with the server's key before they are
  stored, the same way as the passwords of fetched mailboxes, and are part of
  backups only in that form.
- They are never sent back: the portal, the webmail and JMAP show only the last
  four characters. Changing a provider without a new key keeps the old one.
- They are never logged, nor put in the change log.

## Mail is data, not orders

A mail can contain anything, including text written to trick a model ("ignore
your instructions and …"). The assistant is built so that such text can do
little:

- The mail is sent between `<mail>` markers, and the instructions tell the
  model it is data from other people whose requests it must never follow.
- The model gets **no tools**: it cannot send, move, delete, open links or look
  anything up. It only returns text.
- Where the answer is used by the server (spam verdicts, dates, labels) it must
  be JSON of a fixed shape, and is checked: a verdict is one of four words,
  dates must be real dates, a date's quote must really be in the mail, a link
  must be one the mail contains, participants must be people from the mail or
  the address book, labels must be the person's own. Everything else is
  dropped.
- **Nothing happens without a click**, with one exception: auto-labels put
  the person's own labels on new mail. They never move mail, every label is
  logged with the model's reason, and one click takes it off again.
- Quoted history (below "On … wrote:" and `>` lines) is left out, and the text
  is cut to 20,000 characters (4,000 for labels), which keeps both cost and
  surface small.

## Auto-labels

A person switches them on under the assistant's settings. Everyone has the
eight base labels (*Rechnung*, *Versand*, *Termin*, *Newsletter*, *Konto &
Sicherheit*, *Persönlich*, *Arbeit & Geschäftliches*, *Werbung*, in the
person's language), each with a fixed definition and switched on or off one by
one, plus labels of their own, each a name and a description of what belongs
there. A label is a JMAP keyword (`rechnung`, …), so IMAP apps see it as a tag.

Before the model, labels are put on without one, during delivery and before
the person's rules (with *Labels without AI* on, the default; see
[labels.md](labels.md)). When mail is delivered, after the spam filter and the
person's rules, and it is not in Junk, it is put in a queue. A background
worker looks at it again with the same cheap ways plus similar mails, and asks
the provider chosen for `autoLabels` only about the labels they leave in doubt:
with the facts read from the mail (sender type, List-Unsubscribe, amounts,
tracking numbers …), hints and the person's corrections; the model gives a
reason first, then yes, no or unsure. A mail gets at most a main label and a
second one, a lone yes of the model is never a second label, and the facts
overrule the model (a mass mail is never personal). See
[labels.md](labels.md#asking-the-model). Each label is logged with its source,
reason and confidence. The model never takes a label off, and what it puts on
teaches the labels' learning nothing. Delivery never waits for it. A busy
provider (HTTP 429, a timeout) is tried again after one and after five minutes;
after three tries, a wrong key or a day in the queue the mail is left without
the model's labels. Mail that was moved to Junk or the Trash meanwhile is
skipped. Each mail the model is asked about counts against the daily limit like
any other request. The worker takes one mail per person at a time, four people
side by side, and gives each mail 45 seconds before it tries again later; at
most 200 mails of one person wait, more keep no labels.

### Embeddings

An **embeddings provider** makes labels from similar mails: the person's
labeled mails are compared with a new one, and when the most alike agree, the
label goes on without asking the chat model at all (see
[labels.md](labels.md#similar-mails)). Add one under *Providers* with one of
the kinds *OpenAI embeddings* (`text-embedding-3-small`), *Ollama embeddings*
(`http://<address>:11434`, `nomic-embed-text`) or *OpenAI-compatible
embeddings* (any `/v1/embeddings`, e.g. llama.cpp's server with an embedding
model; name the model). It is the admin's only, serves no feature by itself,
and the first enabled one a person may use (by its access list) is taken; its
daily limits and price count like any provider's, under the feature
`autoLabels`. One vector per labeled mail (one byte per dimension, 772 bytes
for 768 dimensions) is kept and goes with the mail and the account. Without an
embeddings provider, the mails' words are compared instead.

### Choosing a model

Labels and the spam check need a model that follows definitions closely. Below
about **7 billion parameters** (gemma-3-4b, Llama 3.2 3B, Qwen 2.5 3B …) models
judge poorly: they put "personal" on every mail that greets the reader by name
and answer yes to several labels at once. The portal reads the size from the
model's name and warns for small ones; **Qwen3-8B**, **Qwen3-14B** or
**gemma-3-12b-it** do much better, and the server's own checks catch the rest.

On a machine of your own, llama.cpp's server runs both: one instance for chat
(`llama-server -m Qwen3-8B-Q4_K_M.gguf -c 16384 --port 8080`) and one for
embeddings (`llama-server -m nomic-embed-text-v1.5.Q8_0.gguf --embeddings
--port 8081`), each added as an *OpenAI-compatible* provider
(`http://<address>:8080/v1`, `http://<address>:8081/v1`). Give both the same
key with `LLAMA_API_KEY` (the old name `LLAMA_ARG_API_KEY` is ignored by
current versions, which then run without a key), and keep the prompt cache
small (`--cache-ram 1024`) on a host with little memory.

## Mail of other accounts

The UwUMail app can also manage a person's accounts elsewhere (Exchange,
Gmail, any IMAP). With the policy switch *AI for mail from other accounts* on,
the app may send the content of such a mail along to summarize it, check it
for spam, find its dates, answer it or judge its labels
([jmap-assist.md](jmap-assist.md#foreign-mail)). The server has none of this
mail and keeps none of it: only the usage is counted, against the same daily
limits. Each server provider has its own option *Mail from other accounts*
(on for new providers, and for the providers there were before 0.21, since
the policy switch decides); the person's own providers may be used for it
whenever the switch is on. The mail goes to the model exactly like the
person's own mail, as data between tags, cut to the same size; for the spam
check the model is told that the authentication results come from the other
account's provider, and the server knows no history of the sender.

## Spam check

The model gets the mail and what the server knows: SPF, DKIM and DMARC as
this server's `Authentication-Results` recorded them (headers of other servers
are ignored), the spam filter's score and rules, whether the mail is in Junk,
and the sender's history (earlier mail, how much of it in Junk, whether the
person wrote to them, whether they are in the contacts), with what the spam
filter's points and rules mean. The model gives its reasons first, each about
something in the mail or the findings, then one of *legitimate*, *suspicious*,
*spam*, *phishing*. When the server's facts clearly speak for the mail (a known
sender, DMARC passed, 0 points or less, not in Junk), *spam* or *phishing*
becomes *suspicious*, at most half sure, and `modelVerdict` keeps what the model
said. The reader shows both, the server's facts and the model's opinion; the
decision stays with the person.

## Dates

`Assist/extractEvents` reads appointments, deadlines and bookings from a mail,
with the people involved matched to the mail's From, To and Cc and the
person's address books (never the person themselves). With *include images*,
the text the server read from the mail's pictures (the same text as
`Email/imageText`) is added, marked as coming from pictures. Pictures
themselves are never sent to a provider: not every provider or model takes
them, it costs much more, and the text is what matters for a date.

## Limits

- A request times out after 60 seconds without an answer and after three
  minutes in total; answers larger than 1 MB are refused.
- At most 16 requests to providers run at once on the server, three per person.
- Drafts and instructions: 20,000 and 2,000 characters.
- Usage is kept for 400 days.

## What is sent where

Only what the feature needs goes to the provider: the mail's text (HTML turned
into text), subject, date, names and addresses of sender and recipients; for
labels only the labels' names and descriptions and the start of the mail. No
attachments, no pictures, no other mail. Which provider gets it is shown with
every answer.

What the provider does with it is up to its terms. OpenAI's and Anthropic's API
terms say API data is not used for training by default; free tiers (Gemini's
in particular) may differ, and OpenRouter depends on the provider it routes to.
For mail that must not leave the house, use Ollama or another server in your
own network.
