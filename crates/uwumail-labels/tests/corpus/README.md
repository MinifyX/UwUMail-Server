# Synthetic labelled mail corpus

Invented mails in German and English for measuring how well labels are put on, without a model
(`tests/integration/corpus.rs`, runs in CI) and with one (`#[ignore]` eval against an
OpenAI-compatible server, see docs/labels.md "Measuring"). Everything here is made up: people,
companies, numbers. Only reserved names are used: domains `example.com`, `example.net`,
`example.org` and `*.example`, `*.test`, `*.invalid`; IPs from 192.0.2.0/24, 198.51.100.0/24,
203.0.113.0/24, 2001:db8::/32. Other tracks (spam) may read it too.

## Format

`mails.jsonl`: one JSON object per line, UTF-8.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | unique, e.g. `de-invoice-003` |
| `lang` | `"de"` \| `"en"` | language of the mail |
| `from` | string | the From address |
| `fromName` | string, optional | the From display name |
| `to` | string[], optional | To addresses (the reader is `max@mail.example` unless said otherwise) |
| `subject` | string | |
| `text` | string | the plain text body (HTML mails as their text), `\n` line ends |
| `headers` | object, optional | further headers, name → value (`List-Unsubscribe`, `List-Id`, `Precedence`, `List-Post`, `Auto-Submitted`, `List-Unsubscribe-Post`, ...) |
| `attachments` | `{name, type}`[], optional | attachments by file name and content type (`text/calendar` for invitations) |
| `auth` | `"pass"` \| `"fail"` \| `"none"`, optional | whether SPF/DKIM/DMARC vouch for the From address (default `pass`) |
| `knownSender` | bool, optional | the reader has the sender in the address book or wrote to them (default `false`) |
| `labels` | string[] | the correct base labels, main label first, at most two; `[]` when none fits |
| `spam` | `"ham"` \| `"spam"` \| `"phishing"`, optional | for the spam track (default `ham`) |
| `note` | string, optional | why this mail is tricky |

Base label ids: `invoice`, `shipping`, `appointment`, `newsletter`, `account`, `personal`, `work`,
`advertising`. Their definitions are in `src/base.rs` (and docs/labels.md); the corpus follows them.
Many mails are deliberately close to a border (a shop's newsletter about free shipping, an order
confirmation with amounts, a company mail greeting the reader by name, a CI notification).
