# Spam filter

The spam filter scores mail that arrives from other servers and decides
whether it goes to the inbox, waits a little, goes to Junk or is refused. It
judges the sending server, the authentication results, what the message itself
shows (its links, attachments and headers) and how the sender behaved before,
and it learns from "Spam" / "Not spam" in the apps. A filter that learns from
the words in messages is next on the [roadmap](roadmap.md).

## What gets scored

Only mail from other servers. Mail submitted by our own people is not scored,
and neither is mail from our own network (private, loopback and link-local
addresses): a scanner or NAS in the house has no reverse name and no DKIM, and
would only collect points for it.

Behind a server in `smtp.trusted_relays`, the filter scores the server that
talked to the relay. Behind a UwUMail Gateway it sees the real address of the
sending server, too.

## What happens

| Score | Outcome |
| --- | --- |
| below `greylist_score` (2.0) | inbox |
| from `greylist_score` to below `junk_score` (5.0) | the sender is asked to come back later (`451`), once; then the message is delivered |
| from `junk_score` (5.0) | Junk |
| from `reject_score` (off) | refused in the SMTP dialogue (`550`) |

Greylisting remembers the sending network (IPv4 /24, IPv6 /64), the envelope
sender and the recipient. A well-behaved server retries after the delay
(`greylist_delay_secs`, 5 minutes) and is let through, and later mail from it
to the same person is not delayed again. Mail that scores below
`greylist_score` is never delayed, so confirmation codes from ordinary
services arrive at once. Setting `greylist_score` to `junk_score` turns
greylisting off.

Refusing is off unless `reject_score` is set: a filter that does not learn yet
is wrong now and then, and Junk loses nothing while a refusal does.

DMARC comes first and does not depend on the score: mail that fails the
sender's `p=reject` policy is refused (unless `smtp.enforce_dmarc_reject` is
off), and `p=quarantine` puts it into Junk.

Mail in Junk is never forwarded to other addresses.

## Rules

### The sending server

| Rule | Points | When |
| --- | --- | --- |
| `DMARC_FAIL` | +2.5 | the From domain publishes DMARC and neither SPF nor DKIM passed aligned with it |
| `SPF_FAIL` | +2.0 | SPF says the server may not send for the envelope sender |
| `DKIM_FAIL` | +1.0 | a DKIM signature is there, but broken |
| `NO_AUTH` | +1.0 | neither SPF nor DKIM passed |
| `HELO_NOT_A_NAME` | +1.0 | the server greeted with an address, a name without a dot, or our own name |
| `NO_REVERSE_DNS` | +1.5 | the sending address has no reverse name |
| `GENERIC_REVERSE_DNS` | +1.0 | the reverse name is made from the address, as on home connections |
| `SPAMHAUS_ZEN` | +4.0 | listed by Spamhaus ZEN |
| `SPAMCOP` | +2.5 | listed by SpamCop |
| `BARRACUDA` | +2.0 | listed by Barracuda |
| `KNOWN_GOOD_SENDER` | −2.5 | at least 5 earlier messages, at most 10 % of them junk |
| `KNOWN_JUNK_SENDER` | +3.0 | at least 5 earlier messages, at least half of them junk |

### What the message contains

The message is read once, on a thread of its own: messages up to 25 MB, the
first 2 MB of their HTML, at most 10 link domains asked about, and zip archives
up to 25 MB looked into without unpacking them. Each rule counts once per
message.

| Rule | Points | When |
| --- | --- | --- |
| `PHISHING_LINK_TEXT` | +3.0 | a link's text shows one site and the link leads to another (only without DMARC, see below) |
| `LINK_TO_IP` | +1.5 | a link leads to a bare IP address |
| `LOOKALIKE_LINK` | +3.0 | a link domain mixes Latin with Cyrillic or Greek letters, or spells a Latin-looking name in them |
| `FROM_NAME_SPOOFS_ADDRESS` | +3.0 | the sender's display name shows an address of another site, like "service@bank.example" |
| `SPAMHAUS_DBL` | +4.0 | a link domain is on Spamhaus DBL as a spam domain |
| `SPAMHAUS_DBL_MALICIOUS` | +6.0 | a link domain is on Spamhaus DBL for phishing, malware or a botnet |
| `SPAMHAUS_DBL_ABUSED` | +1.5 | a link domain is on Spamhaus DBL as a real domain that spammers abuse |
| `EXECUTABLE_ATTACHMENT` | +3.0 | a program, script, shortcut, installer or disk image is attached |
| `MACRO_ATTACHMENT` | +2.0 | an Office file that can carry macros is attached |
| `HTML_ATTACHMENT` | +1.5 | a web page is attached, a common way to bring in a fake login page |
| `DISGUISED_ATTACHMENT` | +2.0 | such a file hides its real ending: "rechnung.pdf.exe", a long gap of spaces, or direction marks |
| `ARCHIVE_WITH_PROGRAM` | +3.0 | a zip archive holds a program or a macro file |
| `MISSING_DATE` | +1.0 | no readable `Date` header |
| `MISSING_MESSAGE_ID` | +1.0 | no `Message-ID` |
| `DATE_IN_FUTURE` | +2.0 | the date is more than a day ahead |
| `DATE_IN_PAST` | +1.0 | the date is more than 30 days old |
| `SUBJECT_ALL_CAPS` | +1.5 | the subject shouts: at least 10 letters, none of them lowercase |
| `HTML_ONLY` | +0.5 | the message has HTML but no plain text version |
| `BASE64_TEXT` | +1.0 | plain ASCII text is base64-encoded, which only hides words from filters |
| `HIDDEN_TEXT` | +1.0 | more than 200 characters of text are hidden from the reader (only without DMARC) |

Newsletters from real senders wrap their links in tracking addresses, so the
text shows the shop while the link leads to the mailing service, and they like
to hide a preview line. Both rules therefore only count for mail that DMARC
does not vouch for. Telling one site from another uses the last two labels of a
name, or three under shared endings like `co.uk` or `github.io`, not the full
public suffix list. The attachment endings are the ones the UwUMail apps ask
about before opening a file.

### Unanswered questions and reputation

A question that could not be answered is worth nothing in either direction:
a blocklist that refuses to answer, a lookup that takes longer than 5 seconds,
or a DNS error.

Reputation counts every delivered message for its sender: by From domain when
the message passed DMARC, otherwise by sending network, because a domain name
that nothing vouches for could be anyone's.

### Examples

A forged message from a server without a reverse name, for a domain that
publishes DMARC, collects `DMARC_FAIL`, `SPF_FAIL`, `NO_AUTH` and
`NO_REVERSE_DNS`: 7.0 points, Junk.

A phishing mail that puts "www.bank.example" on a link to another site, shows
the bank's address as its display name and attaches "rechnung.pdf.exe"
collects `PHISHING_LINK_TEXT`, `FROM_NAME_SPOOFS_ADDRESS`,
`EXECUTABLE_ATTACHMENT` and `DISGUISED_ATTACHMENT`: 11.0 points before anything
about its server counts.

## Spam and Not spam from the apps

When someone moves a message into Junk or out of it, or a mail app sets the
`$junk` or `$notjunk` keyword, the sender's reputation follows. Every delivered
message counts once, when it arrives; marking it moves that count from good to
junk or back. Clicking back and forth never counts twice, and a mistake is
undone by marking the message the other way. The UwUMail apps move the message
and set the keyword together, which also counts once.

Moving spam from Junk to the Trash is tidying up, not "Not spam". Mail that was
never counted (from our own people or network, or from before the filter) has
nothing to move.

## Headers

Every scored message gets two headers, so a mail app can filter on them and a
curious person can see why a message ended up where it did:

```
X-Spam-Score: 7.0
X-Spam-Status: Yes, score=7.0 required=5.0 tests=DMARC_FAIL,SPF_FAIL,NO_AUTH,NO_REVERSE_DNS
```

The server log (Pro mode in the portal) shows the score and the rules for
every received, held back and refused message.

## Blocklists and your DNS resolver

Blocklists are asked through the system resolver. Spamhaus does not answer
queries that arrive through big public resolvers such as Google Public DNS: it
replies with a "you may not ask" code (127.255.255.x), which the filter treats
as no answer. If the server uses such a resolver, Spamhaus ZEN and DBL never
count. DBL is asked about link domains only, never about IP addresses, which it
does not support.
A local resolver that asks the authoritative servers itself (for example
Unbound, or the one in your router if it does not forward to a public
resolver) fixes that.

Spamhaus allows small, non-commercial servers to use its public lists for
free. Bigger or commercial setups need Spamhaus' own terms; UwUMail Server
cannot use their paid query service yet. Turn `spam.blocklists` off if that
applies to you.

## Settings

All of these can be changed in the portal under *Einstellungen* /
*Settings* (the numbers in Pro mode), or in the config file:

```toml
[spam]
enabled = true              # score mail from other servers
blocklists = true           # ask Spamhaus ZEN, SpamCop and Barracuda
junk_score = 5.0            # from here on: Junk
greylist_score = 2.0        # from here to junk_score: hold back once
greylist_delay_secs = 300   # how long a held-back sender waits
# reject_score = 15.0       # refuse from here on; off unless set
```

The server refuses thresholds in the wrong order: `greylist_score` above
`junk_score`, or `reject_score` below it.
