# Moving from mailcow

This is how I move my own mailcow over. It takes over what people set up, so
their apps keep working with the passwords they already have. Mail itself is
copied over IMAP afterwards.

## What comes along

| mailcow | UwUMail |
| --- | --- |
| Domains (active ones, or the ones you choose) | Domains |
| Mailboxes with name, quota and password hash (BLF-CRYPT) | People; the hash is replaced by Argon2 at the first login |
| Mailbox locked for logins | Person locked out (mail still arrives) |
| Aliases to one mailbox | Aliases |
| Aliases to several or outside addresses | Forwarding addresses on the domain page |
| A mailbox address with more targets | Forwarding of that person, already confirmed, keeping a copy if mailcow did |
| Catch-all (`@domain`) to one mailbox | Catch-all |
| App passwords with IMAP, SMTP, DAV rights | App passwords with the same secret and uses |
| Send as `@domain` (sender_acl) | Send as the whole domain |
| Allowed and blocked senders, with wildcards | Sender lists; `*` becomes a pattern |
| Spam scores (low, high) per mailbox | Own limits for Junk and refusing |
| DKIM key and selector | The same key, so the DNS record stays |
| SOGo calendars and address books | Calendars and address books; "personal" becomes the default one |

Left out, with a note in the summary: two-factor logins and passkeys (set up
again), alias domains, `null@localhost` and learning addresses, quarantine,
sieve filters, send-as rights for single addresses, and spam scores for whole
domains.

A person's own settings are only taken over together with their mailbox. If
the person already exists on UwUMail, the import leaves them alone, so running
it again never undoes a change made here.

## 1. Export on the mailcow host

Copy `scripts/mailcow-export.sh` to the mailcow host and run it:

```sh
sudo bash mailcow-export.sh
```

It only reads and writes `/root/uwumail-mailcow-export.jsonl`, readable only
by root. The file contains password hashes, DKIM private keys, calendars and
contacts, so treat it like a password.

## 2. Try it

Stream the file into the running server without storing it there:

```sh
ssh mailcow-host sudo cat /root/uwumail-mailcow-export.jsonl \
  | docker compose exec -T uwumail uwumail-server import mailcow - --dry-run
```

The summary counts what would be created and lists what needs a look. Use
`--domain example.com` (repeatable) to move one domain at a time; I start with
a small one.

## 3. Import

The same without `--dry-run`. Afterwards delete the export on the mailcow host.

## 4. Mail and DNS

Copy the mail over IMAP (coming next), then point the MX and the names mail
apps use (`imap.`, `smtp.`, `mail.`, `autoconfig.`, `autodiscover.`) to the
server or its gateway. Those names join the certificate once they point here.
