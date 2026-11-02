# Verification codes

Sign-in codes and confirmation links are the mail people wait for: they open
the app, copy six digits and forget the mail. UwUMail Server finds the code
when the mail comes in, so a notification can show it, a client can offer it
for autofill, and the mail moves out of the way once the code is no use any
more. It implements Phase 1 of the OMXP draft (`Verification-Info`).

A mail is never refused or held over a code. Past the limit, with guessing
switched off, or when nothing is found, it is delivered like any other and
simply has none.

## The header

A sender that knows the draft writes what the mail is for itself:

```
Verification-Info: kind=otp; origin=login.shop.example; code=482913;
 purpose=login; expires=2026-11-01T17:05:00Z
```

| Field | Meaning |
| --- | --- |
| `kind` | `otp` (a code to type, `code` required) or `link` (a link to open, `url` required) |
| `origin` | the host the code is used on; must lie within the registrable From domain |
| `code` | 4 to 12 characters of `0-9 A-Z -` |
| `url` | an `https` link to `origin` or a host below it, without user name or password |
| `purpose` | `login`, `signup`, `confirm-email`, `reset-password`, `confirm-action` or `other` (default) |
| `expires` | RFC 3339; required |

The header is only believed when a DKIM signature of the From domain covers it
and DMARC passed. Anyone on the way could write one, and a code shown in a
notification and offered for autofill is exactly what a phisher wants to plant.
A header that does not parse, names a foreign origin or has no expiry is not an
error: the mail then goes through the guess below like every other.

Whatever the sender writes, a code is never valid for longer than 24 hours,
counted from when the mail was written or when it arrived, whichever is
earlier. A code that has already expired when the mail arrives is no code: the
mail is delivered without one, and nothing is pushed.

UwUMail Server signs `Verification-Info` in its own outgoing mail, oversigned
like `From` and `Subject`, so a header added on the way breaks the signature
instead of riding along. Because the domain key vouches for it, the header is
removed from everything users submit, like `Face`: otherwise anyone with an
address under a hosted domain could hand out codes the whole domain stands
behind. There is no exception per account yet, so a service on a hosted
domain that wants to send the header has to send through its own signer.

A signed code mail stays valid when it is forwarded: DKIM signs the mail, not
who it is for. Someone who receives a code from a service and forwards it gets
the code shown at the next recipient as well, bound to the same origin. That is
inherent to DKIM; the code only helps whoever holds the account it was sent
for.

## The guess

Most senders don't know the draft yet. For them the server reads the subject
and the start of the text and takes a code only when there is exactly one clear
candidate next to a word like "code" or "Bestätigungscode". Anything that looks
like a date, an amount, a phone number, a postal code or an order, invoice or
customer number is left alone; newsletters with discount codes stay
newsletters. A guessed code is valid for 15 minutes unless the text says
otherwise, and its `origin` is the registrable From domain.

A guess is marked `source: "heuristic"` and is only made when DMARC passed, so
the domain it is bound to is really the sender's: forged mail neither shows a
code nor uses up the real sender's limit. Someone who would rather have no
guesses at all switches them off in the portal (see below); signed headers are
still read.

Mail over 1 MiB is not looked at: a code mail is small, and reading every big
mail again would cost more than it finds.

## Limits and greylisting

At most 10 codes per account and sender (registrable From domain) within an
hour. A service that sends more is either broken or abused to flood someone's
notifications; mail past the limit arrives as normal mail without a code.

A code from a signed header skips greylisting ([spam-filter.md](spam-filter.md)):
it is useless after the retry half an hour later. Each pass counts the moment it
is granted, whether the mail then lands in the Inbox, in Junk, is discarded or
forwarded, and two sessions at once cannot both take the last one. At most 10
passes per account and sender within an hour, and 30 per account across all
senders, so many domains do not multiply the limit. A pass lifts greylisting
only when every recipient of the mail gets one; with one recipient past the
limit or without a code, the mail waits for all of them. Guessed codes wait like
any other mail.

Pushes of codes ([jmap-verification.md](jmap-verification.md)) are limited too:
at most 20 a minute and 60 an hour per account, and a bounded number on the way
at once. The codes past that are kept and shown all the same, just not pushed.

## Clean-up

Once a code expired, its mail moves from the Inbox to the Trash after the delay
the person chose. Flagged mail and mail moved to another folder stay where they
are. Only codes the server trusts are cleaned up: signed headers and guesses
from mail that passed DMARC (today every kept guess), so a false positive or a
phishing mail never takes itself out of sight. The server looks every five
minutes and moves at most 500 mails per transaction.

With clean-up `off`, codes are marked as handled all the same, so switching it
on later does not move old mail at once. A code is forgotten 30 days after its
mail came in; the mail itself stays.

## Settings

In the portal under *Mailbox → Verification codes*, synced to apps as
`UserSettings` ([jmap-settings.md](jmap-settings.md)):

| Portal | Sync key | Values | Default |
| --- | --- | --- | --- |
| Also recognise codes without a label | `verificationHeuristics` | on / off | on |
| Clean up expired codes | `verificationCleanup` | `off`, `immediately`, `24h`, `7d` | `24h` |

## For clients

The code is an Email property and has a query and a Web Push of its own:
[jmap-verification.md](jmap-verification.md).
