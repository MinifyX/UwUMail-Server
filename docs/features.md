# Features

Everything UwUMail Server does, in more words than the [README](../README.md)
has room for, with links to the guides.

## Running it

- **One container.** Mail server, spam filter, admin portal and webmail in a
  single image for amd64 and arm64 (yes, a Raspberry Pi is enough).
  [install.md](install.md), [deployment.md](deployment.md),
  [configuration.md](configuration.md).
- **Guided setup.** A setup assistant walks you through admin account, domain,
  DNS records and sending, checks everything live and ends with a test mail.
- **Delivers from home.** Blocked port 25 or no fixed IP? An optional
  [UwUMail Gateway](gateway.md) on a small VPS tunnels mail and web to your
  server at home and sends from its own address.
- **Backups and updates built in.** Nightly deduplicated, encrypted backups to
  SFTP, an S3 bucket or a folder, single mailboxes restored from the portal
  ([backups.md](backups.md)), and the portal tells you when a new version is
  out and installs it with *Update now*.
- **Everything in one panel.** Accounts, domains, queue, logs, spam and
  settings, in German, English, French, Dutch, Japanese or Chinese, in a
  playful or a plain tone — and with [your own name, logo and colour](branding.md)
  instead of UwUMail's if you like. A calm view with just the traffic light,
  [alert mails](admin-alerts.md), statistics and
  [Prometheus metrics](metrics.md) for admins; login through your own
  [OIDC or LDAP](login-oidc-ldap.md), and [OAuth](oauth.md) for mail apps;
  [TLS reports and DANE](tls-reports.md).

## Mail, calendars and contacts

- **Protocols.** JMAP, IMAP and SMTP for mail apps, CalDAV and CardDAV for
  calendars and contacts, and the same calendars and address books as JMAP
  Calendars and JMAP Contacts for the webmail and the apps;
  [Sieve mail rules](sieve.md) over JMAP and ManageSieve. IMAP speaks IMAP4rev2
  as well as IMAP4rev1. For app developers: [jmap-clients.md](jmap-clients.md).
- **The webmail** under `/mail`, the UwUMail app's interface in the browser,
  with the portal's login ([webmail.md](webmail.md)).
- **Shared calendars and invitations.** Share calendars and address books with
  people on your server; invite anyone to an event and get their answers, in
  the calendar app you already use ([calendars.md](calendars.md)), and see
  when people are free. Over JMAP the whole of JMAP Calendars, down to alerts
  the server rings itself ([jmap-calendars.md](jmap-calendars.md)). iPhone,
  iPad and Mac set everything up with one signed profile.
- **Birthdays.** A birthdays calendar per account from the birthdays and
  anniversaries in the address books, with the age on JMAP, reminders per
  contact, and birthday events from other calendars moved into the contacts
  ([birthdays.md](birthdays.md)).
- **Bring your calendars along.** Import `.ics` and `.vcf` files, subscribe to
  calendars by their iCal address (Google's secret address too), or move
  everything over from iCloud, WEB.DE, GMX, Posteo and other CalDAV/CardDAV
  providers in one go ([calendar-import.md](calendar-import.md)).
- **Push to closed apps.** The webmail notifies with its tab closed, and the
  Android app gets new mail through UnifiedPush ([jmap-push.md](jmap-push.md)).

## Sharing and addresses

- **Shared folders.** Share a folder with people on your server — to read, to
  read and write, or everything — from My account, over JMAP or with IMAP ACLs;
  it shows in their mail app and webmail ([sharing.md](sharing.md)).
- **Groups and shared mailboxes.** Groups like `info@` reach several people,
  shared mailboxes like `support@` are used by a team, and an account can be
  turned into one ([groups.md](groups.md)).
- **Masked addresses** keep your real one away from websites: random addresses
  per website, made in the portal, the webmail, the apps or a password manager,
  on your own domains or on domains kept only for them
  ([jmap-masked-email.md](jmap-masked-email.md)).

## Moving in

- **Copy an old mailbox.** Give the old address and its password under *My
  account → Moving*, and the server copies every folder over in the background
  ([moving.md](moving.md)).
- **Fetched mailboxes.** Keep collecting mail from another provider under *My
  account → Fetched mail*; Outlook.com, Microsoft 365, Gmail and Google
  Workspace sign in with Microsoft or Google instead of a password, and answers
  go out from the fetched address ([fetch.md](fetch.md)).
- **Coming from mailcow?** [migrating-from-mailcow.md](migrating-from-mailcow.md).

## Spam, viruses and privacy

- **Spam filter.** It learns, keeps sender and word lists and fetches known-bad
  lists by itself ([spam-filter.md](spam-filter.md)); an optional
  [ClamAV beside the server](antivirus.md) turns infected mail away before it
  is taken.
- **Remote pictures through the server.** The webmail and the apps get remote
  pictures from the server, or through a VPN, never from the reader's device.
  They are cached for the whole server for a week, sized before they arrive so
  nothing jumps, and dead hosts cost seconds, not the whole mail
  ([jmap-remote.md](jmap-remote.md)).
- **One-click unsubscribes** go out through the server too, only for links the
  sender's DKIM signature covers ([jmap-unsubscribe.md](jmap-unsubscribe.md)).
- **Text in pictures.** Tesseract reads the text in a mail's pictures (posters,
  tickets) so dates in them can be found ([jmap-image-text.md](jmap-image-text.md)).
- **Profile pictures.** A picture for everyone, services, groups and a logo per
  domain, visible on the server or also to other servers over Libravatar and
  the `Face:` header — and sender pictures from the reader's own contacts first
  ([profile-pictures.md](profile-pictures.md)).
- **No telemetry.** Your mail stays on your hardware.

## AI assistant

Drafts and rewrites, summaries of a mail or a conversation, a second opinion on
spam, dates for the calendar and your own labels on new mail — with the
providers you set up (OpenAI, Anthropic Claude, Google Gemini, Mistral,
OpenRouter, Ollama, any OpenAI-compatible server such as LM Studio), for
everyone, some domains or some people, with daily limits per person, prices
from public lists or set by hand, and per provider a switch whether people see
the costs. People may bring their own keys where you allow it. Every AI button
in the webmail and the apps shows the tokens, the cost and what is left today
before the click. Asked by the server, never on without a click except the
labels people switched on ([llm.md](llm.md), [jmap-assist.md](jmap-assist.md)).
