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
otherwise ("gültig für 10 Minuten", "Er gilt 10 Minuten", "verfällt nach zwei
Minuten", "expires in ten minutes", "valable 10 minutes", "válido durante 10
minutos", "valido per 10 minuti", "10 minuten geldig"), and its `origin` is the
registrable From domain. Code words, validity phrases and the words that rule a
number out (order, invoice, customer, tracking, phone, postal code, discount)
are known in German, English, French, Spanish, Italian and Dutch.

What counts as a code:

| Written | Returned | Rule |
| --- | --- | --- |
| `482913` | `482913` | 4 to 8 digits |
| `123 456`, `123-456` | `123456` | groups of at least 2 digits, 4 to 8 in all |
| `G-123456` | `123456` | one capital, a hyphen and 4 to 8 digits (the prefix is never typed) |
| `AB12CD` | `AB12CD` | 6 to 8 capitals and digits, both in it |
| `K7P-2QX` | `K7P-2QX` | 2 or 3 groups of 2 to 5 capitals and digits, both in it, 5 to 10 in all |

The code is returned the way the person types it: digit groups are split for
reading only, so their spaces and hyphens go, and so does a `G-` prefix that the
sign-in field already shows. In a code with letters the hyphen is often part of
what the site expects, so it stays. A group of three or more letters alone
(`SAVE-2026`, `ABC-123`) looks like a word or a product number and is no code;
neither is a word in capitals without digits.

An HTML-only mail is read with each block (paragraph, heading, table cell, `div`)
on a line of its own, so a code standing alone in a cell under "Einmalcode" is
found like the same code in plain text. A code split into one cell or `span` per
digit, or written as `4 8 2 9 1 3`, is put back together; so is a code in the
subject alone.

Without a code, an HTML mail may still carry a sign-in or confirmation link.
The guess takes it, as `kind: "link"` without a code, only when all of this
holds:

- exactly one link sits behind a button such as "Sign in", "Log in",
  "Anmelden", "Verify", "E-Mail bestätigen" or "Passwort zurücksetzen";
- it is an `https` link to the registrable From domain or a host below it,
  without user name or password, and its query carries no other address (a
  click tracker on another domain or a redirect is no such link);
- the text says the link was asked for or expires ("If you didn't request
  this", "nicht angefordert", "This link expires in 10 minutes");
- the mail is no list, bulk or reply mail (`List-Id`, `Precedence: list`,
  `bulk` or `junk`, `In-Reply-To`, `References`): in a platform's notification
  someone else may have written the link.

Only links in the visible HTML count, not those in comments, `head`, `style`,
`script` or `template`. A newsletter's "Jetzt anmelden" for an event says
neither, and stays a newsletter. Like a signed code, a guessed link stays
valid when the mail is forwarded.

A guess is marked `source: "heuristic"` and is only made when the server knows
the From domain is really the sender's, so forged mail neither shows a code nor
uses up the real sender's limit:

| The sender | Guess | `autofill` |
| --- | --- | --- |
| DMARC passed | yes | yes |
| a DKIM signature passed whose domain is the From domain or shares its registrable domain, without a DMARC record | yes | yes |
| logged in here and sent through submission, as the only account on its registrable domain (see below) | yes | yes |
| logged in here and sent through submission, on a domain other accounts share | yes | no |
| only SPF passed, for an envelope sender in the From domain's registrable domain, without a DMARC record | yes | no |
| anything else | no | – |

Why: an aligned DKIM signature proves what DMARC would prove with `p=none`, that
the From domain's key signed the mail; DMARC only adds the policy for what to do
when it fails, and a code is only taken when it passed. Many small services sign
but publish no DMARC record. SPF alone says less: it vouches for the envelope,
the From header is not covered, and on a shared sending service with a loose SPF
record another customer may get the same pass. Such a code is shown, but never
offered for autofill, never pushed and never cleaned up on its own, so a wrong
one stays in sight. When the From domain publishes a DMARC record, alignment
alone counts for nothing: the DMARC result decides, so a domain that asks for
strict alignment is not talked round by a subdomain's pass. Someone who would rather have no guesses at all switches them off in the
portal (see below); signed headers are still read.

## Mail from this server

Mail that apps on this server send through authenticated submission (port 587 or
465) to an account here is looked at as well, when it is delivered: the account
logged in and may send as the From address, which the server already checked, so
it counts like a DMARC pass with the registrable From domain as `origin`. It is
filled in only when the sending account is the only one with addresses on that
registrable domain, counting accounts, aliases, masked addresses, send-as grants,
shared mailboxes and groups their members may send as. On a domain several
people share, or a masked-only domain, a submitted code is only shown, like one
on SPF alone: anyone there could otherwise plant a code for a neighbour's login
on the same domain. There is no flag yet to mark an app account as the
domain's service. The code is only looked for when the mail reaches someone
here. The same goes for
mail forwarded inside the server (forwarding addresses, account forwarding) and
for Sieve `redirect` to a local account: the code found once for the incoming
mail is kept for every local account it reaches. Fetched mail goes through the
same check as mail over SMTP, with the DKIM signature checked again when it is
fetched. A `Verification-Info` header is still removed from submitted mail, so a
local sender's code is always a guess.

Mail over 1 MiB is not looked at: a code mail is small, and reading every big
mail again would cost more than it finds.

## Limits and greylisting

At most 10 codes per account and sender (registrable From domain) within an
hour, and 10 more guesses that are only shown: those count on their own, so a
forger with an SPF pass cannot use up the share of the real codes. A service that sends more is either broken or abused to flood someone's
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
that may be filled in (`autofill`; not those on SPF alone), so a false positive or a
phishing mail never takes itself out of sight. The server looks every five
minutes and moves at most 500 mails per transaction.

With clean-up `off`, codes are marked as handled all the same, so switching it
on later does not move old mail at once. A code is forgotten 30 days after its
mail came in; the mail itself stays.

## After an update

Once after an update to a version whose detector finds more, the server looks in the
background for codes in the mail of the last 24 hours that has none yet, in
batches of 50 with a short pause between them, and notes in the settings that it
is done (`verification-rescan:2`, the detector's version, not the release's),
so neither a restart nor a release that leaves the detector alone starts over. Junk
and Trash are left out. The sender is judged only by the `Authentication-Results`
this server wrote itself, and only for mail its SMTP delivery stored: mail
appended over IMAP, imported or restored may carry a copy of that header that
anybody could have written. Only codes that are still good when the re-scan
runs are kept; one that has run out since is of no use and not stored, so the
clean-up does not move old mail on its account either.

What the re-scan can not see: mail from apps on this server that arrived before
the update was stored without the server's verdicts, so it is skipped; its
codes were good for minutes. A `Verification-Info` header is read as a guess,
since whether its signature covered it is not in the stored results. Codes the
re-scan finds do not count toward the limit of 10 per sender and hour, so a
backlog does not crowd out a live code that comes right after.

## Settings

In the portal on the page *Verification codes* (`/account/codes`), synced to apps as
`UserSettings` ([jmap-settings.md](jmap-settings.md)):

| Portal | Sync key | Values | Default |
| --- | --- | --- | --- |
| Detect codes automatically (DE/EN/FR/ES/IT/NL) | `verificationHeuristics` | on / off | on |
| Clean up expired codes | `verificationCleanup` | `off`, `immediately`, `24h`, `7d` | `24h` |

## For clients

The code is an Email property and has a query and a Web Push of its own:
[jmap-verification.md](jmap-verification.md).
