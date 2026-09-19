# Spam filter

The spam filter scores mail that arrives from other servers and decides
whether it goes to the inbox, waits a little, goes to Junk or is refused. It
judges the sending server, the authentication results, what the message itself
shows (its links, attachments and headers) and how the sender behaved before.
It learns from "Spam" / "Not spam" in the apps, both about senders and about
what spam looks like. Everyone can keep a list of senders that are always let
through or kept out and a list of suspicious words, and so can admins for a
domain or the whole server. Built-in lists from abuse.ch, mailcow and Rspamd
add known malware links and files, spam subjects, throwaway and freemail
domains and link shorteners. A virus scanner can look at every message before
it is taken; that one is not about points at all, see
[antivirus.md](antivirus.md).

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

Refusing is off unless `reject_score` is set: any filter is wrong now and then,
and Junk loses nothing while a refusal does.

Everyone can set their own limits under *Mein Konto → Spamfilter*: from how
many points their mail goes to Junk, and from how many it is refused. Empty
follows the server. The Junk limit may be higher or lower than the server's;
the refusal limit can only be stricter, so a server-wide `reject_score` still
refuses. A message to several people is refused only when it would be
refused for every one of them; the others' limits put it into Junk for those
who wanted it refused.

DMARC comes first and does not depend on the score: mail that fails the
sender's `p=reject` policy is refused (unless `smtp.enforce_dmarc_reject` is
off), and `p=quarantine` puts it into Junk.

Allowed and blocked senders come before the score; see
[Allowed and blocked senders](#allowed-and-blocked-senders).

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
| `BAD_WORDS` | up to +10.0 | entries of the whole server's word list, see [Word lists](#word-lists) |
| `MALWARE_LINK`, `MALWARE_ATTACHMENT`, `DISPOSABLE_FROM`, `FREEMAIL_REPLYTO`, `LINK_SHORTENER` | see there | [built-in lists](#built-in-lists) |

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
`$junk` or `$notjunk` keyword, the sender's reputation follows and the
[learning filter](#the-learning-filter-bayes) learns the message. Every delivered
message counts once, when it arrives; marking it moves that count from good to
junk or back. Clicking back and forth never counts twice, and a mistake is
undone by marking the message the other way. The UwUMail apps move the message
and set the keyword together, which also counts once.

Moving spam from Junk to the Trash is tidying up, not "Not spam". Mail that was
never counted (from our own people or network, or from before the filter) has
nothing to move.

## The learning filter (Bayes)

The Bayes filter learns what spam and wanted mail look like on this server.
For every learned message it counts small pieces, called tokens: words from
the subject, words and word pairs from the text, the sites its links lead to,
the From domain, the mail program, the kind of message and the endings of
attachment names. A new message is taken apart the same way. Tokens that showed up mostly in spam point one way, tokens
from wanted mail the other, and tokens seen only a few times count little. The
150 most telling tokens are combined into a chance that the message is spam
(Robinson and Fisher's method, as in SpamBayes).

| Rule | Points | When |
| --- | --- | --- |
| `BAYES_SPAM` | up to +5.0 | the chance is 80 % or more, +5.0 at 100 % |
| `BAYES_HAM` | down to −3.0 | the chance is 20 % or less, −3.0 at 0 % |

In between it gives no points. The sender reputation and the clear cases below
look at a message's points without the learned rules (these two and the
reputation rules), so what was learned never feeds on itself.

It learns from:

- **Marks.** "Spam" and "Not spam" in the apps (moving into or out of Junk, or
  `$junk` / `$notjunk`) teach the whole server and the person who marked the
  message. Changing one's mind unlearns the earlier verdict first.
- **Clear cases**, for the whole server only: 12 points or more on the
  message's own merits, or DMARC passed with nothing against it and not in
  Junk.
- **Mail that is already sorted**, once and on request: in the portal under
  *Mein Konto → Spamfilter* for one's own mail, under *Server → Spamfilter* for
  everyone's, or with `uwumail-server spam learn [address]`. Mail in Junk
  counts as spam, read mail in the inbox and archive that is older than two
  weeks as wanted mail, at most 2,000 of each per person. This teaches the
  whole server and each person.

The server's knowledge counts once it has learned 50 spam and 50 wanted
messages. Every person also has their own knowledge, only from their own marks
and their own sorted mail. It counts once it reaches 50 and 50 as well and is then mixed with the
server's: 60 % their own at first, growing to 90 % by 500 learned messages.
That way the same newsletter can land in one person's inbox and in another
one's Junk. `uwumail-server spam stats` shows how far the server got.

Tokens are not stored as text. Each one is hashed with a secret key of the
server (HMAC-SHA-256, cut to 64 bits), so the database holds numbers instead
of words, and without the key nobody can check whether a word was learned.
Learning runs in the background. Tokens seen only once and not for 90 days are
forgotten, and what a message was learned as is forgotten after a year.

With `spam.bayes` off, the filter gives no points and does not learn clear
cases, but it keeps learning from marks, so it is ready when it is turned on
again.

## Allowed and blocked senders

Everyone keeps their own list of allowed and blocked senders in the portal
under *Mein Konto → Spamfilter*. Admins keep one for the whole server and one per
domain under *Server → Spamfilter*, or on the command line.

| Kind | Example | Matches |
| --- | --- | --- |
| IP address or network | `192.0.2.10`, `198.51.100.0/24`, `2001:db8::/48` | the address of the sending server; networks may be at most /8 (IPv4) or /16 (IPv6) wide |
| Host name | `mx1.example.com`, `*.mail.example.com` | the reverse name of the sending server, but only if that name points back to the same address; `*.` matches the names below, not the name itself |
| Email address | `news@example.com` | the From address; blocking also looks at the envelope sender |
| Domain | `example.com` | the From domain and its subdomains; blocking also looks at the envelope sender's |
| Pattern | `*.tld`, `*newsletter*`, `*@example.com` | the whole From address, `*` standing for any text; blocking also looks at the envelope sender. Needs three characters besides `*` |

The portal and the command line guess the kind: an address or network, a `*.`
host name with a dot after it (`*.mail.example.com`), anything else with `*` as
a pattern, anything with an `@`, otherwise a domain. A single host name without
`*.` has to be chosen as a host name. Patterns are what Mailcow and Rspamd call
wildcards in their allow and block lists.

| List | A person's | A domain's or the server's |
| --- | --- | --- |
| Allowed | inbox | inbox |
| Blocked | Junk | refused in the SMTP dialogue (`550 5.7.1`) |

Allowed senders are never held back by greylisting, and their score cannot
refuse them or put them into Junk. A DMARC quarantine does not either, but a
failed `p=reject` policy is still refused. An allowed From address or domain
only counts when SPF or DKIM passed for that domain (or a parent or subdomain
of it), with or without a published DMARC policy, because anyone can write
any From. An address or a confirmed host name of the sending server cannot be
faked that way and always counts. Blocking needs no confirmation.

Which entry decides:

1. Within each list (the person's, the domain's, the server's), the most
   specific matching entry: an email address before a single IP address or
   exact host name, before a network or `*.` host name, before a domain, and a
   subdomain before its parent, before a pattern, a longer pattern before a
   shorter one. At a tie, blocking wins. So `boss@example.com`
   can be allowed while `example.com` is blocked in the same list.
2. If the server's or the domain's list blocks, the message is refused, no
   matter what a person allowed.
3. Otherwise a person's own list decides, then the domain's, then the server's.

The domain is the one of the address the message was sent to. When a message
goes to several people and a domain list blocks it for only some of them, the
others get it and the blocked ones find it in Junk, because the SMTP dialogue
can only refuse a message for everyone at once. Changes to the server's and
the domains' lists are in the change log.

```sh
uwumail-server spam block 198.51.100.0/24 --note "only ever sent spam"
uwumail-server spam allow news@example.com --domain example.org
uwumail-server spam block mx1.example.net --kind host
uwumail-server spam block '*.tld'
uwumail-server spam allow grandma@example.net --account someone@example.org
uwumail-server spam senders [--account someone@example.org]
uwumail-server spam unlist 12 [--account someone@example.org]
```

## Word lists

Words, phrases and regular expressions that make a message suspicious. Like
sender lists, there is one per person (*Mein Konto → Spamfilter*), one per
domain and one for the whole server (*Server → Spamfilter*), and the command
line has `uwumail-server spam words`.

| Entry | Example | Matches |
| --- | --- | --- |
| Word or phrase | `casino`, `web development` | whole words in any case, with any whitespace between them |
| Expression | `/\sviagra\s/i` | a regular expression with the flags `i`, `m`, `s`, `x` (and `u`, which changes nothing), like Rspamd's regexp maps |

Each matching entry gives 2.5 points unless it was added with points of its
own, and all word lists together give at most 10. The subject and the text a
reader sees are searched, HTML turned into text, up to 200 KB. The server's
list counts as `BAD_WORDS` for everyone and shows in the headers; a domain's
and a person's own lists count for that recipient only, within the same 10
points.

Expressions run on Rust's regex engine, which takes linear time whatever the
pattern: a list cannot slow the server down. In exchange, look-around and
back-references are not supported, and an expression that matches an empty
text is refused, because it would match every message.

Entries can be typed in one per line, or a whole list can be pasted, for
example an Rspamd map. Lines starting with `#` are skipped; the portal and
the command line report what was added, what was already on the list and which
lines could not be used, and why.

A list can also be subscribed to by link. The server fetches it right away and
then every day, only over https, only from public addresses, without following
redirects, at most 1 MB, and only when it changed. Its entries replace those of
the last fetch; a fetch that fails or brings nothing usable keeps them. A
subscription can look in the subject only, for lists of spam subjects.

```sh
uwumail-server spam words add casino "web development" --points 3
uwumail-server spam words import bad_words.map --domain example.org
uwumail-server spam words subscribe https://lists.example.org/bad.map
uwumail-server spam words list [--account someone@example.org]
uwumail-server spam words remove 12
uwumail-server spam words unsubscribe 3
```

## Built-in lists

Lists the server fetches itself, each of which can be switched off under
*Server → Spamfilter* or in the config file (`[spam.feeds]`). Fetching means
the server contacts these providers regularly, similar to asking blocklists.
The lists are not part of UwUMail: every server fetches them from their
providers under their terms.

| List | Provider | Fetched | Rule | Points |
| --- | --- | --- | --- | --- |
| Malware links (`urlhaus`) | [abuse.ch URLhaus](https://urlhaus.abuse.ch/api/) | hourly | `MALWARE_LINK`: a link in the message is a known malware address that is online | +10.0 |
| Malware attachments (`malware_bazaar`) | [abuse.ch MalwareBazaar](https://bazaar.abuse.ch/export/) | hourly | `MALWARE_ATTACHMENT`: an attachment's MD5 or SHA-256 was reported as malware in the last two days | +10.0 |
| Spam subjects (`bad_subjects`) | [mailcow](https://github.com/mailcow/mailcow-dockerized) | daily | counts like the server's word list, in the subject only | +2.5 each |
| Throwaway addresses (`disposable`) | [Rspamd](https://rspamd.com/) | daily | `DISPOSABLE_FROM`: the From domain or a parent of it is a throwaway service | +1.5 |
| Freemail providers (`freemail`) | [Rspamd](https://rspamd.com/) | daily | `FREEMAIL_REPLYTO`: replies are meant to go to a freemail address although the sender has none | +2.0 |
| Link shorteners (`redirectors`) | [Rspamd](https://rspamd.com/) | daily | `LINK_SHORTENER`: a link goes through a shortener or redirector | +0.5 |

- **abuse.ch** needs one's own Auth-Key from
  [auth.abuse.ch](https://auth.abuse.ch/). Its
  [terms](https://abuse.ch/terms-of-use/) allow free use for non-commercial
  purposes only; commercial use needs a paid subscription. Without a key the
  two lists stay off. The key is stored like a password and only goes into the
  download address.
- **mailcow's** list is only reachable over plain http. So that a changed list
  cannot sort ordinary mail into Junk, expressions that match everyday subjects
  ("Hallo", "Rechnung September", …) are dropped when it is read.
- **Links** are never opened: `LINK_SHORTENER` only looks at the host name.
  Reading a malware address list compares whole addresses, without the part
  after `#`.

A fetch that fails, or brings no usable entries, keeps the values of the last
good one and is tried again an hour later. The portal shows for each list how
many entries it holds, when it was fetched and why it failed, with a button to
fetch it now; `uwumail-server spam feeds` shows the same.

## Headers

Every scored message gets two headers, so a mail app can filter on them and a
curious person can see why a message ended up where it did:

```
X-Spam-Score: 7.0
X-Spam-Status: Yes, score=7.0 required=5.0 tests=DMARC_FAIL,SPF_FAIL,NO_AUTH,NO_REVERSE_DNS
```

With the virus scanner on, every message that is taken also says whether
anyone looked at it:

```
X-Virus-Scanned: yes (ClamAV)
```

The server log in the portal shows the score and the rules for every
received, held back and refused message.

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

All of these can be changed in the portal under *Server → Spamfilter*,
together with checking senders and following `p=reject`, or in the config
file:

```toml
[spam]
enabled = true              # score mail from other servers
blocklists = true           # ask Spamhaus ZEN, SpamCop and Barracuda
bayes = true                # the learning filter
junk_score = 5.0            # from here on: Junk
greylist_score = 2.0        # from here to junk_score: hold back once
greylist_delay_secs = 300   # how long a held-back sender waits
# reject_score = 15.0       # refuse from here on; off unless set

[spam.feeds]
urlhaus = true              # built-in lists, see above
malware_bazaar = true
bad_subjects = true
disposable = true
freemail = true
redirectors = true
# abuse_ch_key = "..."      # from auth.abuse.ch, non-commercial use only
```

The server refuses thresholds in the wrong order: `greylist_score` above
`junk_score`, or `reject_score` below it.
