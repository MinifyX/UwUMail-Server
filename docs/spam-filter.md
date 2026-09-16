# Spam filter

The spam filter scores mail that arrives from other servers and decides
whether it goes to the inbox, waits a little, goes to Junk or is refused. It
does not read the message text yet: this first part judges the sending server,
the authentication results and how the sender behaved before. Content rules
and a filter that learns from "Spam" / "Not spam" in the apps are next on the
[roadmap](roadmap.md).

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

A question that could not be answered is worth nothing in either direction:
a blocklist that refuses to answer, a lookup that takes longer than 5 seconds,
or a DNS error.

Reputation counts every delivered message for its sender: by From domain when
the message passed DMARC, otherwise by sending network, because a domain name
that nothing vouches for could be anyone's.

A forged message from a server without a reverse name, for a domain that
publishes DMARC, collects `DMARC_FAIL`, `SPF_FAIL`, `NO_AUTH` and
`NO_REVERSE_DNS`: 7.0 points, Junk.

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
as no answer. If the server uses such a resolver, Spamhaus ZEN never counts.
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
