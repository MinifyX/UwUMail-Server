# TLS reports and DANE

Mail between servers is encrypted with STARTTLS, but on its own STARTTLS only
encrypts when both sides feel like it: somebody in between can strip the offer,
and a sender that accepts any certificate cannot tell the real server from a
stand-in. Two standards close that gap, and a third lets both sides find out
whether it worked:

- **MTA-STS** (RFC 8461): the receiving domain publishes on the web that it
  wants TLS with a certificate valid for its MX host. See
  [deployment.md](deployment.md#mta-sts-and-reports).
- **DANE** (RFC 7672): the receiving domain publishes in DNS, signed with
  DNSSEC, which key its MX host's certificate has. No certificate authority is
  involved, and nothing can be faked without breaking the DNSSEC signatures.
- **TLS reports** (RFC 8460): once a day, the sender tells the receiving domain
  how many connections were encrypted as its policy wants and which ones failed,
  and why.

UwUMail does all three in both directions. This page is about the parts that
came with 0.14: reports this server sends, and DANE.

## Reports we send

Every delivery to another domain's MX host counts once, by the UTC day: under
which policy it went (`sts` for MTA-STS, `tlsa` for DANE, `no-policy-found`)
and whether TLS worked. When it did not, the reason is kept in the words of
RFC 8460 (`starttls-not-supported`, `certificate-expired`,
`certificate-host-mismatch`, `certificate-not-trusted`, `validation-failure`,
`tlsa-invalid`, `dnssec-invalid`, `sts-policy-fetch-error`,
`sts-policy-invalid`, `sts-webpki-invalid`), together with the MX host and both
IP addresses. A policy that could not be fetched counts too: a domain that
announces MTA-STS but serves no usable policy hears about it. Domains in MTA-STS
*testing* mode get their certificate problems reported, as that mode asks,
while the mail still goes out.

Within the hour after a UTC day is over, the server looks up
`_smtp._tls.<domain>` for every domain it delivered to that day. Domains without that record get nothing. The others
get one report per day, as JSON packed with gzip:

- `rua=mailto:…` addresses get it by mail through the normal queue, from
  `noreply-tls-reports@<domain>`, where `<domain>` is our domain the server's
  host name belongs to (or the host name itself, if it belongs to none), signed
  with that domain's DKIM keys. The mail has the
  subject, headers and attachment name RFC 8460 asks for, and the empty envelope
  sender of a bounce, so a report that cannot be delivered does not come back.
- `rua=https://…` addresses get it as a `POST` with
  `Content-Type: application/tlsrpt+gzip`, straight from the server (not
  through the egress proxy, since the report names the server anyway), only to
  public addresses, and without following redirects.

A report that could not be sent is tried once more the next day, with the same
report id. At most five addresses per domain get one; a domain's own list with
more is cut there. Days older than three days are not reported any more, e.g.
after the server was off. Our own domains never get a report, and neither do
addresses at them.

The sessions of a day are kept for eight days, the list of sent reports for 60.
Per domain and day, at most 100 different kinds of failure are told apart; past
that they are only counted, without hosts and addresses, so a domain with ever
new MX hosts cannot fill the database.

In the portal, *Accounts & domains → Reports → Reports we send* lists what went
out, to where, and why a report could not be sent. The switch there is the same
as the setting below.

```toml
[reports]
send_tls_reports = true   # count deliveries and send daily TLS reports (the default)
```

As an environment variable: `UWUMAIL_REPORTS__SEND_TLS_REPORTS=false`. Switched
off, deliveries are not counted either.

## DANE for mail we deliver

Before delivering to a domain, the server asks whether the domain's MX records
are signed with DNSSEC and validate. Only then does it look up the TLSA records
of each MX host at `_25._tcp.<host>`, again with validation. The answers are
validated on this server itself, against the root zone's trust anchor, so a
resolver that does not validate is fine as long as it passes the signatures on.

What happens then:

| What DNS says | Delivery |
| --- | --- |
| MX not signed, or no TLSA records | As before: TLS whenever the other side offers it, MTA-STS if the domain has it |
| TLSA records with usage 3 (DANE-EE) or 2 (DANE-TA) | STARTTLS is required, and the certificate must match one of them. DANE comes before MTA-STS. |
| TLSA records, but none usable (e.g. only PKIX usages 0 and 1) | STARTTLS is required, the certificate is not checked |
| MX records or TLSA records whose signatures do not validate | That host gets no mail; the mail waits in the queue and is tried again later |

DANE-EE (`3 x x`) matches the server's own certificate or its public key,
whatever names and dates the certificate carries. DANE-TA (`2 x x`) matches a
certificate the server sends along in its chain; the chain from there down must
hold, and the server's certificate must be valid for the MX host name or the
recipient's domain. A certificate no record matches ends the connection with
`tlsa-invalid` in the domain's TLS report; unvalidatable signatures give
`dnssec-invalid`. Both leave the mail in the queue: RFC 7672 wants a sender to
wait rather than fall back to an unchecked connection.

If DNSSEC answers do not validate here at all (a resolver or firewall that
strips the signatures), the server notices at the first delivery, logs it, and
delivers without DANE, checking again every 15 minutes. The same goes for a
single lookup that fails or times out. Mail is never held back only because this
server's own DNS is broken.

## DANE for mail to us

When the zone of the server's host name is signed with DNSSEC, the DNS check of
the domain the host name belongs to recommends a TLSA record:

```text
_25._tcp.mail.example.com. TLSA 3 1 1 <SHA-256 of the server's public key>
```

`3 1 1` names the key, not the certificate, so it stays valid when the
certificate is renewed, as long as the key stays the same. With Let's Encrypt,
the server therefore keeps the key of its certificate (`data/tls/acme/key.pem`)
and only asks for a new certificate for it. A self-signed certificate
(`tls.mode = "self-signed"`) keeps its key as well; with your own certificate
files (`tls.mode = "files"`) keeping the key is up to you. No record is
recommended while the server still waits for its first Let's Encrypt
certificate, since that one's key is not known yet.

The check also compares a published record with the certificate the server
presents right now:

- It matches: fine. The portal shows a red reminder next to it, because from
  now on the key must not change unannounced.
- It matches nothing: this is marked wrong and counts for the domain's status,
  so the server overview turns red. Servers that check DANE hold mail for all
  domains on this server back until the record is right again.
- The zone is not signed: the record is harmless but has no effect yet, and the
  check says so.
- The name's DNSSEC signatures do not validate: marked wrong, because validating
  senders then cannot reach the server at all.

### Changing the key

The key changes when you switch to your own certificate files or to another
certificate authority, when you move to another server without its data
folder, or when you delete `data/tls/acme/key.pem` on purpose (the next renewal
then comes with a new key). Do it in this order:

1. Get the new key's record. For a key file,
   `openssl pkey -in key.pem -pubout -outform DER | openssl dgst -sha256`
   gives the hash after `3 1 1`.
2. Publish the new record **next to** the old one, and wait at least twice the
   record's TTL (a day is safe).
3. Change the key. The DNS check then shows the new record as the one that
   matches.
4. Remove the old record.

The Cloudflare button never touches TLSA records: they have to change in step
with the server's key, which only you can time.
