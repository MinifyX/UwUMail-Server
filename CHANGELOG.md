# Changelog

Each release gets a section here before its tag is pushed; CI copies the section into the GitHub
release. Versions follow semver; `-beta.N` versions are pre-releases.

## 0.18.0

**Fetched mailboxes at Microsoft and Google sign in there** ([docs/fetch.md](docs/fetch.md#microsoft-and-google)):

- Microsoft has switched passwords off for IMAP and SMTP at Outlook.com, Hotmail and most Microsoft
  365 tenants (`NO Basic authentication is disabled.`). Fetched mailboxes there now sign in with
  OAuth 2 and log in with SASL XOAUTH2, for fetching and for answering from the address.
- *Mein Konto → Abrufkonten* recognises Microsoft and Google addresses (their own domains, and
  Microsoft 365 and Google Workspace by the domain's mail servers) and offers *Mit Microsoft
  anmelden* / *Mit Google anmelden* first, a password as the fallback. The sign-in is proven with a
  real login before it is saved.
- **Microsoft** works out of the box: the device code flow with the client ID UwUMail ships with —
  a big code, a copy button and the link to microsoft.com/devicelogin; the server asks Microsoft at
  its interval. Admins can set their own Entra app under *Einstellungen → Anmeldung*
  (`fetch.oauth.microsoft_client_id`).
- **Google** uses the authorization code flow with PKCE and the admin's own *Web application*
  client (`fetch.oauth.google_client_id`, `fetch.oauth.google_client_secret`), with the way back
  (`/api/account/fetch/oauth/callback`) tied to the browser that set off.
- Refresh and access tokens are sealed like provider passwords, renewed before they run out, a
  rotated refresh token is kept, and a provider that is down is asked again with a growing wait. A
  grant the provider ended stops the mailbox, shows *Anmeldung abgelaufen – erneut anmelden* and
  sends its owner a notice once. All requests to the providers go through the egress, with the
  proxy and its fallback when fetching takes it.
- Microsoft's "Basic authentication is disabled" is its own error, not a wrong password: runs stop
  asking, the owner is told once, and the row offers *Auf Microsoft-Anmeldung umstellen*. Mailboxes
  that still log in with a password at Microsoft or Google can switch under *Bearbeiten*.
- New endpoints under `/api/account/fetch`: `provider`, `oauth/start`, `oauth/flows/{flow}`,
  `oauth/callback`; creating and editing a fetched mailbox take `oauthFlow`. Migration
  `0056_fetch_oauth.sql`.

## 0.17.0

**Profile pictures** ([docs/profile-pictures.md](docs/profile-pictures.md)):

- Everyone can set a picture of themselves under *My account* (and in the webmail's settings);
  admins set them for services, shared mailboxes and groups, and one logo per domain stands in for
  every address of the domain without a picture of its own. Pictures are cut square, scaled to at
  most 512 × 512 and stored anew without the file's metadata.
- Who sees it is chosen per person: nobody, people on this server (the default) or everyone. An admin
  can forbid "everyone" for the whole server or per domain.
- "Everyone" means other mail servers and apps: the server is a **Libravatar** provider at
  `/avatar/<hash>` (the DNS check recommends the `_avatars-sec._tcp` record), and a person can have
  a small copy sent along in every mail's **`Face:` header** (off by default). Masked addresses never
  get or reveal a picture.
- **Sender pictures per address:** `pictureUrl` answers with the reader's own contact photo first,
  then the person's picture (someone on this server, a `Face` from mail a DKIM signature of the From
  domain covers, or Libravatar where the domain publishes it — never Gravatar), then the domain logo
  and company logos as before. New JMAP extension `urn:uwumail:jmap:profile`
  (`ProfilePicture/get` and `/set`); `pictureUrl` takes `source=logo` and `local=1`.
- Contact photos that are only a link are fetched by the server through the egress, never by the
  browser.

**One-click unsubscribe** ([docs/jmap-unsubscribe.md](docs/jmap-unsubscribe.md)): `Email/unsubscribe`
sends a newsletter's RFC 8058 one-click POST from the server, only when a valid DKIM signature covers
`List-Unsubscribe` and `List-Unsubscribe-Post`, only to public https addresses, through `[egress]`
like remote pictures, without cookies, referrer or redirects; at most once per message every five
minutes and 30 an hour per login.

**JMAP Calendars complete** ([docs/jmap-calendars.md](docs/jmap-calendars.md)):
`Principal/getAvailability`, `CalendarEventNotification`, `CalendarEvent/copy` and `/parse`,
`queryChanges` for queries that expand recurrences, drafts, custom time zones, invitations to single
instances, default alerts (also as CalDAV default alarms), alerts rung by the server (a
`CalendarAlert` push or a reminder mail), and per-person properties of shared calendars and events:
name, colour, order, visibility, availability and alerts stay each person's own and never touch the
owner's data.

**Webmail 0.12.0:** pictures in the contact editor (choose, drop, paste, camera, company logo, crop
to a circle), sender pictures per address everywhere a person appears, *Settings → Profile picture*,
one-click unsubscribe through the server.

**Portal:** *Updates* is its own entry in the server menu instead of a tab of the overview.

**Security** ([docs/security-audit-0.16.0.md](docs/security-audit-0.16.0.md),
[docs/security-audit-0.17.0.md](docs/security-audit-0.17.0.md)): every open Low finding of the 0.16.0
audit is fixed, and a review of everything new found and fixed one critical and three high issues
before release.

- OAuth: self-registered apps nobody allowed are forgotten after a day and can no longer fill the app
  table; errors only go back automatically to apps allowed before or on the device (no open
  redirect); allowing a new OAuth app or forwarding to another server asks for the password again.
- Mail forging one of our own domains that DMARC did not stop is no longer forwarded to other
  servers; unproven own-domain senders get SRS. Outgoing mail carries no lone CR or LF (SMTP
  smuggling). The two-`From` and header checks run on every incoming message, fetched mail included.
- Shared folders no longer show the owner's activity elsewhere in JMAP states, change lists, push or
  IMAP `HIGHESTMODSEQ` (now per folder); a narrowed share applies from the next IMAP command.
- Push subscriptions only reach https on port 443. Free/busy answers only for a person's own
  addresses (never masked ones) and only to people of the same domains or who share a calendar.
  Control characters from other accounts can no longer break CalDAV/CardDAV sync.
- Single-mailbox restore reads a snapshot within strict limits; changing the SFTP backup host or
  user asks for the password again.
- Gateway: one IPv6 /48 holds at most 100 connections (`max_connections_per_ipv6_site`); unpaired
  tunnel peers get one stream, 256 KB and a few handshakes. The root helpers print no terminal escape
  sequences, and the OpenVPN file filter is an allowlist.
- New code: a time zone rule or contact photo value with multi-byte characters, or an out-of-range
  day, could stop the server (fixed before release); invitations from elsewhere no longer bring the
  organizer's alarms along, and reminder mails are limited; our DKIM signatures cover `Face`;
  private events show only their times to people they are shared with, also over CalDAV, and those
  people can't change or delete them.
- Webmail 0.12.0: invitations, cancellations and answers in a mail only count from the organizer or
  an invited participant (WEBMAIL-2); links in events always go through the link check (WEBMAIL-3).
- The health overview names domains that nobody can make masked addresses on since the 0.16.0
  upgrade (MD-1).

**Upgrading:** migrations 0049–0055 run on the first start (calendar per-person data, drafts,
notifications, event versions, alerts, per-folder change counters, profile pictures) and index
existing contact photos once. Worth knowing:

- People a calendar is shared with keep their own name and colour for it now; renaming it for
  everyone is the owner's.
- Invitations from elsewhere lose the organizer's alarms; set your own or use default alerts.
- JMAP apps of people with shared folders reload those accounts once
  (`cannotCalculateChanges`).
- Push subscriptions to other ports than 443 stop receiving pushes.
- To let other servers find public pictures, add the `_avatars-sec._tcp` SRV record the DNS check
  suggests.

## 0.16.0

**Domains only for masked addresses** ([docs/jmap-masked-email.md](docs/jmap-masked-email.md#switching-it-on)):

- A domain can be **masked-only**: chosen when adding it (*Add domain → Only masked addresses*,
  `uwumail-server domain add --masked`), or by turning a domain into one that carries nothing but
  masked addresses (`domain kind <domain> masked`). It has no people, aliases, groups, forwarding
  addresses or catch-all; DKIM, DNS records, MTA-STS and reports work as for any domain, and
  postmaster@ and abuse@ still reach the admins. Turning it back into a mail domain is always
  possible.
- Per domain, the admin decides where its people may make masked addresses: **off**, on **their
  own domain**, on **masked-only domains** (which ones is chosen per domain) or **both**, and which
  domain a new one gets when the app does not say (password managers). Each part can be set
  differently for a single person on their page.
- *My account → Masked addresses* offers exactly the allowed domains. Over JMAP, the MaskedEmail
  capability of the account lists them with the default, and `MaskedEmail/set` takes an optional
  `domain` (a UwUMail addition; Fastmail's apps are unaffected).
- The webmail (0.11.0) has a page for masked addresses under *Settings → Masked addresses*: make,
  copy, switch off, delete and restore them, with the domain choice.
- A message to two masked addresses of the same person turns both on and notes it on both; before,
  the second stayed pending and was deleted a day later.

**Upgrading:** a domain that was open for masked addresses becomes "own domain" for **its own**
people. People of other domains can no longer make new ones there; the ones they made keep working.
A domain that only carried masked addresses (nobody's login is on it) therefore offers them to nobody
after the upgrade: make it masked-only (*its page → Make it masked-only*) and choose it under the
mail domains' masked address settings. `PUT /api/admin/domains/{domain}/masked-addresses` is replaced
by `…/masked-policy` and `…/kind`.

**Security** ([docs/security-audit-0.16.0.md](docs/security-audit-0.16.0.md)): a full audit of
server and webmail, and every finding from Medium up fixed, 42 in all. Most were ways for one input
to take the whole server down or make it use far too much memory or CPU:

- A deeply nested message sent to port 25 crashed the server, and again at every retry (critical).
  Every message now passes a cheap check of its shape before it is parsed: at most 64 levels, 5,000
  parts and 20,000 header fields ([configuration.md](docs/configuration.md#limits-on-the-shape-of-a-message)).
- Crashes from broken dates (IMAP APPEND before login, invitations, CalDAV), from a user-chosen IMAP
  server's `LIST` answer, and from deep `bodyStructure`; memory bombs in `Email/set`, JMAP result
  references, IMAP FETCH and SEARCH; quadratic loops in the spam filter and when reading a website
  for a sender picture.
- **Logins:** one limiter for every protocol (portal, IMAP, ManageSieve, SMTP, JMAP, DAV), counted
  before the password is checked, and a server-wide cap on password checks running at once, which
  answers "try again later" when it is full. Ten failures over IMAP now also hold that network off
  the portal ([deployment.md](docs/deployment.md)).
- **Ids are never handed out twice:** a purged account's id went to the next new account, and an
  IMAP or WebSocket connection still open from the old one then acted on the new one. IMAP,
  ManageSieve, the JMAP WebSocket and event stream now end with their login (password change,
  revoked app password or OAuth app, disabled account).
- App passwords and OAuth tokens limited to `mail` no longer reach calendars and contacts over JMAP;
  that takes `dav`. Tokens from `/jmap/token` made before 0.16.0 are `mail`-only and see calendars
  again once the app asks for a new one.
- Scheduled mail is cancelled when the login that scheduled it ends or the account is disabled.
- Turning a person into a service or shared mailbox also removes their forwarding, fetched
  mailboxes and moves, and switches off their active Sieve script, calendar subscriptions and
  masked addresses.
- JMAP keywords must be IMAP atoms, so nobody can put fake IMAP answers into another person's mail
  app; bad ones already stored are removed.
- **Limits:** JMAP uploads count against the quota (1 GiB a day at most), mail methods have
  per-request bounds ([jmap-clients.md](docs/jmap-clients.md#limits)); one calendar change mails
  at most `max_recipients` outside people; SMTP allows 20 connections per client address
  (`smtp.max_connections_per_client`; raise it if a proxy hides client addresses) with deadlines;
  IMAP closes connections that do not log in within three minutes; word lists, calendar moves,
  free-busy, S3 listings and the IMAP client of fetch and moving are bounded.
- Fetch and moving connect only to the public address they checked; DANE can no longer be switched
  off with a forged MX answer; a TLS-RPT record can name at most five report addresses.
- Folder backups never follow symlinks on the target; the gateway no longer counts failed
  handshakes against the paired server; its installer and `scripts/deploy-gateway.sh` were
  hardened.
- Webmail 0.11.0: attachment names, links and `List-Unsubscribe` addresses are checked in linear
  time; before, a crafted mail could freeze the tab.

## 0.15.0

**Turning accounts into shared mailboxes** ([docs/groups.md](docs/groups.md#turning-an-account-into-one)):

- An existing person or service becomes a shared mailbox with *Turn into a shared mailbox* on its
  page, members chosen right there; mail, folders, addresses and storage limit stay. A person's
  password lives on as an app password, so their mail apps keep working.
  `uwumail-server account shared <address> on --member … --sender …` does the same.
- *Make it a plain service* turns a shared mailbox back into a service: the members lose it, mail,
  addresses and app passwords stay.
- A shared mailbox is a service with members now: it has **app passwords** and protocol switches, so
  a scanner, a shop or a mail app can log in as it over SMTP, IMAP, JMAP and DAV, while nobody
  signs in to the portal or the webmail as it. IMAP or JMAP has to stay on.
- A shared mailbox may be a member of a group.

**Services stay out of the portal, harder** ([docs/configuration.md](docs/configuration.md#accounts-people-and-services)):

- A service never takes a password kept at an LDAP directory, for the portal or for mail apps.
  Before, a person from the directory turned into a service still got into IMAP, SMTP and JMAP with
  the directory's password.
- Setting a password for a service is refused (`serviceAccount`); services get app passwords only.
- Becoming a service also removes password links, apps signed in with OAuth and the tie to an LDAP
  directory or OpenID Connect provider, and the folders and shared mailboxes others shared with the
  account (only people share with people).
- Tests for every way in: portal, webmail, OpenID Connect, LDAP and OAuth.

## 0.14.0

The "Later" list of the roadmap, almost all of it in one release.

**Moving from another provider** ([docs/moving.md](docs/moving.md)), under *My account → Moving*:
the old address and its password (an app password where the provider wants one) are all it takes.
The server finds the provider's IMAP server the way fetched mailboxes do, logs in once, and copies
every folder in the background with its dates and read, flagged and answered marks. The special
folders go into ours, and nothing comes twice (by Message-ID or content, so a Gmail message with
several labels comes once). It works in five-minute turns, continues after a restart, and pauses
with a plain reason when the mailbox here is full or the password was refused. *Sync again* later
fetches only what arrived since, and *Done, delete login* forgets the sealed password. Gmail,
Outlook.com, GMX and WEB.DE get hints about app passwords and switching IMAP on. It connects only
over TLS to public addresses, through the egress proxy when fetching takes it, and starting or
continuing a move is limited to ten an hour per person.

**Groups, shared mailboxes and masked addresses** ([docs/groups.md](docs/groups.md),
[docs/jmap-masked-email.md](docs/jmap-masked-email.md)):

- **Groups** (*Accounts & domains → a domain → Groups*): one address such as `info@` or `vorstand@`
  for several people.
  - Every member gets the mail in their own mailbox, with their own spam filter, sender lists, Sieve
    rules and forwarding. A full mailbox is only that member's loss.
  - A group can be open to anyone, only to its members, or only to its domain. Other senders are
    refused with `550 5.7.1`, and mail from elsewhere must pass SPF or DKIM to claim a member's
    address.
  - Members can be allowed to send as the group, and find its address among their identities.
  - Over JMAP, groups are principals of type `group`.
- **Shared mailboxes** (*Accounts → Shared mailboxes → New shared mailbox*): a mailbox such as
  `support@` with its own storage that nobody signs in to.
  - Its members see every folder of it, new ones included: in mail apps under `Shared/<address>/`,
    and in the webmail as an account of its own.
  - Members who may send answer with its address, and a copy lands in its Sent folder.
- **Masked addresses** (*My account → Masked addresses*, once an admin opens a domain for them): a
  random address like `maple.otter482@example.org` for each website.
  - On, it delivers to the Inbox. Off, mail goes quietly to the Trash. Deleted, mail is refused, and
    the address is never handed out again.
  - Password managers can make them through Fastmail's JMAP MaskedEmail extension
    (`https://www.fastmail.com/dev/maskedemail`). Addresses they made that get no mail within a day
    go away by themselves.
  - One can reply as a masked address.

**Signing in with OAuth** ([docs/oauth.md](docs/oauth.md)): mail apps that speak OAuth sign in
without an app password. The server is an OAuth 2.0 / OpenID Connect provider: apps register
themselves (public clients only), open the portal, where the person logs in as always – second
factor and passkey included – and agrees, and get an access token for an hour and a refresh token
that is swapped on every use; one that comes back after it was swapped ends the sign-in and the
person gets a mail. PKCE (S256) is required, redirect addresses are https, the device itself or
an app scheme, and tokens are kept as hashes only. IMAP, SMTP submission and ManageSieve take the
token as OAUTHBEARER or XOAUTH2, JMAP, CalDAV and CardDAV as a Bearer token, each within the
scopes `mail`, `smtp` and `dav`. Every app shows up under *My account → Security* and on the
person's page for admins, and can be signed out there.

**Logging in with OpenID Connect or LDAP** ([docs/login-oidc-ldap.md](docs/login-oidc-ldap.md)),
under *Server → Settings → Login*:

- **OpenID Connect:** a *Log in with …* button for Authentik, Keycloak, Authelia and the like.
  The code flow with PKCE, state and nonce; the ID token is checked against the provider's keys.
  A login finds its account by the provider's subject, the first time by a verified address, and
  can make new accounts in chosen domains, admins by a group claim. A second factor set up here
  is still asked for. Requests leave over https to public addresses only.
- **LDAP:** the directory's password in the normal login form (ldaps or STARTTLS), found by a DN
  template or a search with a service account, with filters and names escaped and empty passwords
  never sent. Accounts can be made at the first login or moved to the directory by an admin; mail
  apps may use the directory password as long as main passwords are allowed.
- Both have a *Test connection* button. Secrets from the admin panel (these, and the relay, Loki
  and proxy passwords) are now stored sealed in the database.

**New mail with the app or the browser closed** ([docs/jmap-push.md](docs/jmap-push.md)): the server
now speaks JMAP `PushSubscription` (RFC 8620, 7.2). The webmail uses it for notifications with the
tab closed, and the Android app uses it through a UnifiedPush distributor instead of a foreground
service.

- **What leaves the server:** only which data changed and its new state, never a sender or a
  subject. The message is encrypted for the device (RFC 8291) and signed with the server's own key
  (VAPID, RFC 8292), which the JMAP session announces (RFC 9749).
- **How subscriptions work:** each new subscription must first send back a code the server pushed to
  it. It lasts a week at a time and is renewed by the app. It belongs to the login that made it (the
  webmail's session, an app password or the password) and ends with it.
- **When pushes go out:** changes are bundled for two seconds, with at most one push every five
  seconds. New mail (unread, not in drafts, sent, junk or trash) arrives as `EmailDelivery` with
  high urgency.
- **Where they go:** only to public `https://` addresses, straight from the server, without
  redirects. A push service that no longer knows a subscription (404, 410) ends it; other failures
  are tried again later.
- **Webmail files:** its service worker is served at `/mail/sw.js`, always fresh and allowed to talk
  to this server only.

**Backups to S3 or a folder, and single mailboxes back** ([docs/backups.md](docs/backups.md)):
besides an SFTP server, backups now go to an S3 bucket (Amazon, MinIO, Backblaze B2, Hetzner Object
Storage, Wasabi and others; free endpoint, path or virtual-host style, Signature Version 4, plain
http only inside the own network) or to a folder such as a mounted disk. Settings from before stay
SFTP. Under *Server → Overview → Backups → Restore one mailbox* an admin opens a snapshot, picks a
person and all or some folders, and their mail comes back while the server keeps running, into a new
folder "Restored <date>" next to what is there, checked against its hash and without anything the
mailbox still has. For admins the same as `uwumail-server backup restore-mailbox --account LOGIN
[--folder …]`. `backup restore` also takes `--s3 s3://bucket/folder` and `--folder PATH`.

**A calmer server overview, alerts and statistics** ([docs/admin-alerts.md](docs/admin-alerts.md)):

- **Simple or everything:** each admin chooses at the top of *Server → Overview*. *Simple* is one
  traffic light made of every health area and open alert, only what is yellow or red with what to
  do about it, and shortcuts to people, domains and backups; the rest of the menu folds away under
  *More*. Admins who never chose keep seeing everything.
- **Alerts:** every five minutes the server looks at itself — the health areas (DNS, TLS and DMARC
  reports, certificate, sending, queue, disk, virus scanner, second factors) plus failed or old
  backups and certificate renewals failing for a day — and keeps what it finds as alerts. Admins
  get a mail in their own language and tone when something is new or gets worse, once a day while
  it stays red (until someone clicks *Got it*), and once more when it is fine again; each admin
  picks all, only problems, or none.
- **Statistics** under *Server → Statistics*: mail received and turned away (unknown recipients,
  spam, viruses, rules, greylisting), sent, delivered, retried and given up, failed logins per
  protocol and storage, for 30 days or 12 months as bar charts and as a table. Only numbers per
  day are kept, for 400 days.
- **Prometheus metrics** ([docs/metrics.md](docs/metrics.md)): `GET /metrics` with accounts,
  domains, storage and disk, queue, the statistics' counters, health per area, open alerts,
  certificate expiry, last backup, uptime and version. Off by default; switched on under
  *Server → Statistics* or with `[metrics]`, it needs a bearer token (`metrics.token`), allowed
  networks (`metrics.allowed_networks`) or both, and never answers everyone.

**TLS reports to other domains and DANE** ([docs/tls-reports.md](docs/tls-reports.md)):

- **Reports we send:** every delivery to another domain's mail server counts by the day, with the
  policy it followed (MTA-STS, DANE or none) and whether TLS worked. Once the day is over, domains
  that ask for it with a `_smtp._tls` record get a TLS report (RFC 8460): by mail from
  `noreply-tls-reports@` your domain, DKIM-signed, or posted to their https address, to public
  addresses only. Your own domains never get one. *Accounts & domains → Reports → Reports we send*
  lists what went out and why a report could not be sent; `[reports] send_tls_reports` (on) switches
  it off.
- **DANE for mail we send:** domains whose MX records and TLSA records validate with DNSSEC get
  their
  mail only over STARTTLS with the certificate those records name (DANE-EE or DANE-TA, before
  MTA-STS). Records that do not validate hold the mail back instead of sending it unchecked; a
  resolver that strips DNSSEC only turns DANE off, with a note in the log.
- **DANE for mail to us:** in a DNSSEC-signed zone the DNS check recommends `TLSA 3 1 1` for the
  server's key, and Let's Encrypt renewals now keep that key so the record stays valid. A published
  record that no longer matches the certificate turns the overview red, and the portal reminds you
  to publish the new record before the key ever changes.

Migrations 0040 to 0046 add the tables of moves from other providers (0040), groups and shared
mailboxes (0041), masked addresses (0042), TLS report counts and sent reports (0043), OAuth apps,
sign-ins, tokens and logins at other providers (0044), daily statistics and alerts (0045) and push
subscriptions (0046).

**Updating.** `cd /opt/uwumail && sudo bash update.sh`, or *Update now* under *Server → Updates*, is
all it takes: the migrations run by themselves when the server starts, and there is nothing to set.
A few things work from the start: the statistics count, admins get alert mails (the first look after
the update may send one about what was already yellow or red; each admin picks *All*, *Only
problems* or *None* below the alerts on the overview), mail to domains with DANE records is sent
DANE-checked, and domains that ask for TLS reports get them (`[reports] send_tls_reports` turns that
off). Everything else waits to be switched on or used. Secrets saved in the admin panel from now on
are stored sealed, so going back to 0.13 afterwards means entering them again. The webmail moves to
the merge commit of its pull request #9, which brings notifications with the tab closed, also for
mail in shared mailboxes. The UwUMail app 0.5.0-beta.4 uses the new push on Android through
UnifiedPush.

## 0.13.0

**Calendars and contacts from elsewhere** ([docs/calendar-import.md](docs/calendar-import.md)), under
*My account → Calendars & contacts → Bring them over*:

- **Files:** `.ics` and `.vcf` files up to 20 MB go into a new or an existing calendar or address
  book. They are cut into single entries the way CalDAV keeps them (an event with its exceptions and
  time zones), vCard 2.1 becomes 3.0, and entries without a UID get a lasting one, so importing a
  file twice changes nothing twice. A report lists what was left out and why. For admins the same as
  `uwumail-server import ics|vcf FILE --account LOGIN`.
- **Subscribed calendars:** an iCal address (`https://` or `webcal://`), such as holidays, a club's
  dates or a Google calendar's secret address, fills a calendar the server fetches again every 15
  minutes to once a day, asking only for news (ETag, Last-Modified). Such a calendar is read-only
  over CalDAV (403, no `write-content` privilege) and JMAP (`myRights`), stays out of free-busy,
  invitations and the default, and keeps its entries when the feed answers with an error or anything
  that is not a calendar. The address is stored sealed and only its host is ever shown or logged.
  Reminders of feeds are dropped unless asked for.
- **Moving from another provider:** the address and an app password find the CalDAV and CardDAV
  servers (known providers such as iCloud, WEB.DE, GMX, Posteo, mailbox.org and Fastmail, the
  domain's SRV/TXT records, `.well-known`) and take every calendar and address book over in one
  request; the password is kept nowhere. Google and Outlook.com, which offer no way in for a server
  of one's own, are recognized and explained.
- All of it leaves like fetched mail (`egress.fetch`), over https to public addresses only, with
  every redirect checked again; a login never follows a redirect to another site. 30 such requests
  per person and hour.

Migration 0039 adds the table of subscribed calendars.

**Updating.** `cd /opt/uwumail && sudo bash update.sh`, or *Update now* under *Server → Updates*,
is all it takes: migration 0039 runs by itself when the server starts, and there is nothing to set.
The webmail stays at the commit 0.12.2 pinned; it already shows subscribed calendars as read-only,
since it follows `myRights`.

## 0.12.2

**Remote pictures in Firefox.** The webmail showed no remote pictures at all in Firefox and the
browsers built on it (Zen, LibreWolf, …): the server fetched them fine, but the frame a message is
shown in allowed pictures from `'self'`, and Firefox reads `'self'` there as `about:srcdoc` rather
than the webmail's own address, so it blocked every one. The frame now names the webmail's address
itself. Chrome, Safari and the UwUMail apps were not affected. The webmail is pinned to its commit
with the fix.

## 0.12.1

**Remote pictures through the VPN again.** With the VPN or a proxy set, the server looks up a
picture's address itself and has the proxy connect there. On a machine with IPv6 the first
addresses tried were IPv6 ones, often every one of them for a CDN. The VPN usually has no IPv6,
and gluetun drops such a connection silently instead of refusing it, so each try waited ten
seconds and the picture gave up: no remote pictures and no sender logos in the webmail or the apps
while the VPN was on. Through a proxy IPv4 is now tried first, IPv6 only after it. Without a proxy
nothing changes.

## 0.12.0

**Programs sign in with a token.** An app password with the *mail* use now also works as
`Authorization: Bearer <token>` on every JMAP endpoint (session, API, upload, download, push).
Programs can get one themselves: `POST /jmap/token` trades login, password and, with two-factor
authentication, a code for a new named app password. It shows up in *My account → Security* like
any other, with the usual notice mail, and wrong tries are throttled like Basic logins
([docs/jmap-tokens.md](docs/jmap-tokens.md)).

**JMAP catches up.**

- **Push over WebSocket** (RFC 8887) at `/jmap/ws`: requests, responses and push with
  `WebSocketPushEnable`/`Disable` and `pushState`, also through the gateway.
- **Undo send on the server:** every JMAP submission waits for the person's undo window (off, 5,
  10, 20 or 30 seconds; default 10) and can be taken back with `undoStatus: canceled`. The window
  is set in *My account → Forwarding & sending* or as `undoSendSeconds`.
- **Send later:** `EmailSubmission` with `sendAt` or FUTURERELEASE (`HOLDFOR`/`HOLDUNTIL`) up to
  30 days ahead. Held mail is kept in the database and survives restarts
  ([docs/jmap-sending.md](docs/jmap-sending.md)).
- **`Email/copy`** between your own account and accounts that share folders with you, within the
  folder rights.
- **`/queryChanges`** for Email, Mailbox, EmailSubmission, SieveScript, ContactCard and
  CalendarEvent (without `expandRecurrences`), with `canCalculateChanges: true`.
- **Signatures** of every sending address can be edited in the portal; they are the ones JMAP
  `Identity` serves.
- **Address suggestions** (`AddressSuggestion/query`, `urn:uwumail:jmap:suggest`): recipients
  from the address books and recent mail, ranked ([docs/jmap-suggest.md](docs/jmap-suggest.md)).
- **Tested with other programs:** aerc and Fastmail's JMAP-TestSuite
  ([docs/jmap-clients.md](docs/jmap-clients.md)). That turned up and fixed: session URLs on
  `https://localhost` for HTTP/2 clients, uploads answering `201` (aerc could not send; now `200`),
  Mailbox/query without AND/OR/NOT filters, paging and tree options, Mailbox/set refusing unchanged
  server-set properties, and several details of Email/get and Email/set (line endings, address
  names, `us-ascii` for parts without a charset, grouped addresses, dates with offset, header forms
  on create, `notFound` in `blobNotFound`).

**IMAP4rev2 and shared folders.** IMAP speaks IMAP4rev2 (RFC 9051) beside IMAP4rev1: `ENABLE
IMAP4rev2`, ESEARCH answers, no RECENT, UTF-8 mailbox names; also BINARY, UNAUTHENTICATE and
SEARCHRES. Folders can be shared with people on your server — to read, to read and write, or
everything — from *My account → Addresses and storage*, over JMAP (`shareWith`) or with IMAP ACLs
(RFC 4314). IMAP shows them under `Shared/<person>/`, JMAP as an extra account per person with
`myRights` and Principals, and the webmail as "Shared by …". Mail filed there counts against the
owner's storage ([docs/sharing.md](docs/sharing.md)).

**Calendars.**

- **Invitations** (iTIP/iMIP): people on the server get invitations, updates and cancellations
  straight into their calendars, everyone else by mail in the sender's language, and answers come
  back into the organizer's event. Invitations arriving by mail land in the default calendar,
  waiting for an answer; answers and cancellations only count when SPF/DKIM vouch for the sender.
- **CalDAV scheduling** (RFC 6638) for Apple Calendar, Thunderbird and DAVx5, and
  `sendSchedulingMessages` in JMAP Calendars.
- **Shared calendars and address books** between people on the server (read, read and write,
  everything) over CalDAV, CardDAV and JMAP, with push, from the new page *My account → Calendars &
  contacts* ([docs/calendars.md](docs/calendars.md)). `shareWith` uses the principal ids of
  Principal/get (`p12`) for mailboxes, calendars and address books alike.
- **Signed Apple profiles:** the configuration profiles for iPhone, iPad and Mac are signed with
  the server's certificate and show as "Verified". With a self-signed certificate they stay
  unsigned.

**Spam filter: SURBL and URIBL.** The link blocklists `multi.surbl.org` and `multi.uribl.com` can
be switched on next to the other blocklists (`spam.uri_blocklists`, off by default: they are free
only for small servers and do not answer through public resolvers). The registrable domain of up
to 8 links per message is asked; the answers become `SURBL_PH`, `SURBL_MW`, `SURBL_ABUSE`,
`SURBL_CR`, `URIBL_BLACK`, `URIBL_RED` and `URIBL_GREY`. "Query refused" answers never count and
are logged once.

**Gateway: one button for Cloudflare.** In the setup assistant and under *Server → Overview → Mail
flow*, a button points the host name's A/AAAA records at the gateway's addresses (not proxied),
along with `mta-sts`, `autoconfig`, `autodiscover`, `mail`, `imap` and `smtp` names that already
are A/AAAA records. It shows what would change first and only replaces addresses that point
elsewhere after you confirm.

**Roadmap.** An optional external Rspamd and external mailboxes as JMAP accounts are no longer
planned.

Migrations 0036 (held submissions), 0037 (folder sharing) and 0038 (calendar sharing and
Schedule-Tag) run by themselves on the first start.

The webmail is [UwUMail Webmail 0.10.0](https://github.com/MinifyX/UwUMail-Webmail/tree/v0.10.0):
undo send and send later from the server with a "Scheduled" view, signatures per sending address,
recipient suggestions, folders and calendars others share, "Share…" for your own, push over
WebSocket and Accept / Maybe / Decline for invitations.

## 0.11.0

**Your own name, logo and colour.** *Server → Settings → Branding* turns UwUMail into your
server's own ([docs/branding.md](docs/branding.md)):

- **Logo:** PNG, JPEG, WebP or SVG up to 512 KB. It replaces Nyu in the sidebar, on the login
  page, in the webmail and in the browser tab. It is kept in the database, so it is in every
  backup, and served in a sandbox where an SVG cannot run scripts.
- **Name:** shown instead of "UwUMail" in the portal and the webmail, and as the sender of the
  mail the server writes (security notices, bounces to your own people, forwarding confirmations,
  the test mail). Autoconfig, Apple profiles, authenticator apps and passkeys show it too.
- **Accent colour:** pick one of eight presets or any colour. Every shade for the light and the
  dark theme is worked out from it and moved until buttons and links read at WCAG 4.5:1 or
  better; a preview shows both before saving. Without a chosen colour nothing changes, and the
  pink stays exactly as it was.
- **Nyu and the kaomoji** can be switched off: no cat anywhere, no faces, and the portal, the
  webmail and the mail to your own people all in the plain tone.

The settings are also `brand.name`, `brand.color` and `brand.mascot` in the config file and on the
command line.

**Six languages.** The portal, the webmail and the mail the server writes now also speak French,
Dutch, Japanese and Simplified Chinese, beside German and English, in both tones. "Same as the
browser" picks the first of the browser's languages the server speaks. Bounces, security notices,
forwarding confirmations, the test mail, the names of the first calendar and address book and
the pages on port 80 are translated too. The language setting under *Mail from the server* and
the synced `language` preference take `fr`, `nl`, `ja` and `zh`.

**Accounts & domains.** *Server → Accounts & domains* gathers *Accounts*, *Domains* and *Reports*
under one entry with tabs, like the other pages since 0.10.0. The addresses stay the same.

The webmail is [UwUMail Webmail 0.9.0](https://github.com/MinifyX/UwUMail-Webmail/tree/v0.9.0).

## 0.10.0

**A tidier server menu.** The admin part of the portal had grown to thirteen entries. It now has
eight, and the pages that belong together share one page with tabs, like the spam filter:

- *Server → Overview* has the tabs *Overview*, *Mail flow*, *Backups* and *Updates*. The overview
  shows a tile for each of them — whether mail gets through, when the last backup ran, whether a
  new version or a system update is waiting — and the last change anyone made; a click opens the
  tab. *Mail flow* is what used to be *Setup*: reachability, gateway, sending route, reverse DNS,
  blocklists and a test message. The setup assistant itself is only for the first start now.
  The card for the machine and its helper moved to *Updates*.
- *Server → Logs* has the tabs *Live log*, *Changes* (the change log) and *Log shipping*
  (Grafana Loki).
- *Server → Settings* has the tabs *General*, *Sending & receiving*, *Apps & webmail* and
  *VPN & proxy*, which used to be a page of its own.

Every tab has its own address. Old bookmarks keep working: `/admin/setup`, `/admin/log` and
`/admin/vpn` lead to their tabs, and the hints in the health overview open the right tab.

## 0.9.3

**Everything from the portal.** Nothing about the VPN or an update needs the command line any more,
once the machine's helper is version 3:

- *Server → Updates* has an *Update now* button. After the password the helper fetches `update.sh`
  from the newest release, checks its checksum and runs it on the machine: backup, `compose.yaml`,
  images, and back to the old version when the new one does not come up (shown as *rolled back*).
  The page follows its output while the server is replaced and offers to reload once it is done.
- The helper updates itself: *Server → Overview* offers *Update helper* when there is a newer one,
  and `update.sh` now always takes the helper along, from the portal and from the command line.
- *VPN & Proxy* has *Remove VPN* next to *Switch the VPN off*: it takes out the container,
  `.env.vpn`, the OpenVPN file and the keys stored in the portal, and sends everything straight
  again. Switching off keeps the settings for next time, as before.

What the helper takes from the server stays a verb from a fixed list — never a command, a path, an
address or a version. An update is always the newest release from GitHub, checked against its
`sha256`, so a server somebody took over can ask for the newest UwUMail and nothing older or
elsewhere. This revises A-6 of the 0.4.0 review, which kept updates to a person on the machine.

**One last time on the machine:** a helper from before this release cannot update itself yet. The
portal shows the command — `cd /opt/uwumail && sudo bash update.sh` — which brings UwUMail and the
helper up to date together; after that everything happens in the portal.

`update.sh` now ends with exit code 3 when it rolled back, instead of 1.

## 0.9.2

**Provider files are recognised.** Reading a WireGuard `.conf` under *VPN & Proxy* now tells from
the file whose it is — NordVPN (`Endpoint = …wg.nordhold.net`), Mullvad, Proton VPN, Surfshark,
IVPN, AirVPN, Windscribe — switches to that provider and takes over the country from the server's
name (a NordVPN file named `de1380-nordvpn.conf` sets *Germany*). Before, the file was read into
whatever provider happened to be selected and its location was left out. A file nobody knows is
taken as an own server, and a server named in it instead of an IP address is now looked up when
saving rather than refused.

**The VPN can always be switched off.** *Switch the VPN off* only showed while gluetun reported
`running`, so a VPN that kept restarting could not be stopped from the portal, and without a new
enough helper switching off was refused altogether. The button now shows whenever anything of the
VPN is left, sends everything straight again at once, and asks the helper to stop gluetun where
there is one. A job the helper has not started yet shows as *waiting*, and the page follows it
until it is done, instead of going on showing the state from before.

## 0.9.1

**Only names nobody owns in examples.** Tests, docs, sample data and placeholders used
`example.de`, `verein.de`, `shop.de` and a few other invented names under real endings, which
somebody owns and whose mail servers would get anything sent there by mistake. They now use the
names reserved for this (`example.com/net/org`, `.example`, `.test`, `.invalid`) and addresses from
the documentation networks; [CONTRIBUTING.md](CONTRIBUTING.md) says so for the future. Real names
stay only where they are real: provider lists, blocklists, public suffixes and the tests that check
those.

**A flaky test fixed.** The two JMAP push tests waited a fixed 200 ms for their event source, which
was too short on a busy machine. They now wait for the server's answer, after which the
subscription is guaranteed to be in place.

Nothing changes for a running server.

## 0.9.0

**VPN & proxy in the portal.** *Server → VPN & Proxy* sets up the VPN for remote pictures without
touching a file: pick the provider (NordVPN, Mullvad, Proton VPN, Surfshark, IVPN, AirVPN,
Windscribe, every other provider gluetun knows, or an own WireGuard or OpenVPN server), paste the
key or read the provider's `.conf` or `.ovpn` file, choose countries or cities, and press *Save and
connect*. The machine's helper (`deploy/host`, now version 2) writes `.env.vpn`, adds `vpn` to
`COMPOSE_PROFILES` and starts gluetun; the portal follows it with the log and points the way out at
it. Without the helper the portal shows `.env.vpn` and the command to copy. Keys are stored in the
database and never sent back to the browser; the helper passes on only gluetun's own variables,
only values without quotes or line breaks, and refuses `.ovpn` directives that start programs or
read files.

The way out can now be changed while the server runs, and each kind of request that tells about
readers takes it by choice: remote pictures and sender logos (on), the check for new versions and
fetching from other mailboxes (both off by default). A proxy of one's own (`http://` or
`socks5://`) and the fallback are set on the same page. New settings `egress.pictures`,
`egress.updates` and `egress.fetch`; the empty `UWUMAIL_EGRESS_*` variables `compose.yaml` passes on
no longer lock the setting. See [docs/configuration.md](docs/configuration.md#remote-pictures-through-a-vpn).

**The spam filter, rearranged.** The page that showed every setting and every list one below the
other is now tabs — overview, rules, settings, lists, learning, viruses, history — and the overview
says at a glance how many rules there are, which ones decide the most, and blocks or allows a
sender in one line. Allowed and blocked senders and words are **one table of rules**, searched,
filtered, sorted and paged by the server, so a server with many domains, people and thousands of
entries stays usable:

- search by value, note, domain or person; filter by scope (the whole server, all domains, all
  people, or one of them from a searchable list with counts), effect, kind and state;
- every rule can be edited in place and moved to another scope, and many at once can be allowed,
  blocked, moved, given an end date or removed;
- rules can **run out** (for 1, 7, 30, 90 or 365 days, or until a date) and are removed by
  themselves;
- every rule counts its **hits** and when it last decided something, so rules nobody needs any
  more are easy to find;
- **import** pasted lines or a text or CSV file (up to 20,000 lines, senders with `allow`/`block`
  and a note per line) and **export** what the filters show as CSV;
- admins look after people's own rules too.

My account gets the same table for one's own rules. Migration 35 adds the end date and the hit
counters. The API is `/api/admin/spam/rules` and `/api/account/spam/rules`, see
[docs/spam-filter.md](docs/spam-filter.md#the-portal). The older sender and word endpoints stay.

## 0.8.0

**Contacts over JMAP** (webmail [v0.8.0](https://github.com/MinifyX/UwUMail-Webmail/releases/tag/v0.8.0)).
The address books people already keep over CardDAV are now JMAP Contacts too
(`urn:ietf:params:jmap:contacts`, RFC 9610), the way the calendars became JMAP Calendars in 0.7.0:
address books with a default one per account, and cards that travel as JSContact over JMAP and
stay vCards on disk, so a phone over CardDAV and the webmail over JMAP see the same contacts, with
changes and push for both. The webmail and the apps get a contacts section beside mail and
calendar, with "add to contacts" for senders and attached vCards, and recipient suggestions name the
address books first. Migration 34 gives every account a default address book. See
[docs/jmap-contacts.md](docs/jmap-contacts.md).

**Pictures in mail come through the server.** A picture loaded from its sender tells them the mail
was opened, when and from where. The server now fetches a mail's remote pictures and a company
sender's logo for its readers (`/jmap/image` and `/jmap/picture`, announced as
`urn:uwumail:jmap:remote`, see [docs/jmap-remote.md](docs/jmap-remote.md)), and the webmail and
the apps use it; the webmail's own policy no longer lets the browser load a picture from anywhere
else. Only public addresses are fetched, redirects are checked again, and a picture has to be one.
These requests — and only these — can go through a VPN: `compose.yaml` has an optional `gluetun`
service behind the `vpn` profile, set up with the `UWUMAIL_EGRESS_*` values in `.env.example`, or
any HTTP-CONNECT or SOCKS5 proxy under `[egress]`. While the proxy is away, pictures wait (`block`,
the default) or go directly (`direct`). The admin panel shows it under Server settings → *Pictures
in mail*, with a button that tests the way out. Mail delivery, DNS and blocklists keep leaving
directly.

**Reviewed before release.** Everything since 0.7.1, and a fresh read of the whole server, had a
security review ([docs/security-audit-0.8.0.md](docs/security-audit-0.8.0.md)): two High and
fourteen Medium findings, all fixed with a test, the Low ones listed with their reasons.

- A backup target can no longer switch a backup's encryption off, or hand a restore a database of
  its choosing: with a key configured, only encrypted objects are accepted, and every object is
  checked against its own content. Names from the target's listing are checked and every read has
  a limit, so a broken or hostile target can't bring the server down either.
- A `BDAT` chunk is measured against the message size limit before it is read, as `DATA` always
  was. `BDAT 0 LAST` is answered at once.
- A `From` that names addresses in more than one domain is refused like two `From` headers are,
  because DMARC has nothing to say about it. The client's `HELO` goes into this server's own
  `Received` header only as a host name or an address literal.
- Signing in: a successful login forgives only its own failures. After ten wrong passwords a login
  gets one try every 30 seconds; after ten wrong second-factor codes the account's second factor
  rests for 15 minutes and the owner gets a notice. Web, IMAP, SMTP and ManageSieve share this.
- The web ports close connections that send nothing for 20 seconds and take at most 4096 at once,
  128 from one network (behind a reverse proxy only the total counts).
- The DNS check now recommends a CAA record that lets only this server's Let's Encrypt account
  issue certificates for its name, so a compromised gateway VPS can't get one. The Cloudflare
  button writes it only when ticked. [docs/gateway.md](docs/gateway.md) explains why.
- The root helpers on the gateway and the host no longer touch files by name in directories the
  other side can write to.
- IMAP `LIST` patterns, mailbox discovery, the fetch worker, contact and calendar queries, and the
  sender-picture cache all have limits now that could be run up before.
- The webmail's review ([v0.8.0](https://github.com/MinifyX/UwUMail-Webmail/releases/tag/v0.8.0),
  its docs/security-audit-2026-09.md) fixed two ways a crafted mail could freeze the tab.

The webmail's attachment previews for text, tables, calendar files, contacts and PDFs now show on
a real server: its policy blocked them before, which nobody had noticed because the demo has none.
A reopened reply draft keeps its threading, and embedded pictures in the reader use their
attachment directly.

## 0.7.1

**Images follow dark mode** (webmail [v0.7.1](https://github.com/MinifyX/UwUMail-Webmail/releases/tag/v0.7.1)).
When a mail is shown dark, newsletter images with white paper baked in are recoloured to match: the
paper takes the colour behind the image, text and lines turn light, colours keep their hue. Photos,
large shapes and light text on coloured buttons stay as they are. The work happens in the browser,
in a worker, so large images never make the page stutter. The webmail can read embedded images and
remote ones whose server allows it; the others stay as the sender made them. It is on by default
and can be switched off under *Lesen*.

The settings extension takes the new key `darkImages`, so that switch follows the account between
the webmail and the apps.

## 0.7.0

**The webmail gets a calendar, mail rules and folders** (webmail [v0.7.0](https://github.com/MinifyX/UwUMail-Webmail/releases/tag/v0.7.0)).
A switch at the top of the sidebar leads from the mail to a calendar with month, week and day views
and an agenda on the phone: click or drag to make an event, drag it to move or stretch it, open it
for the full editor with place, notes and repeats, and choose "only this one" or "the whole series"
when deleting from a series. Calendars keep their colour, can be hidden, renamed, made the default
or deleted. Under Settings → Rules, mail can be sorted as it arrives — by sender, recipient, subject
or mailing list into a folder, marked as read, flagged, moved to the trash or passed on — and the
editor writes the account's Sieve script for it. Folders can be made, nested, renamed and deleted
from the sidebar, and Trash and Junk have a button that empties them.

**Calendars over JMAP.** The calendars people already keep over CalDAV are now JMAP Calendars
too (`urn:ietf:params:jmap:calendars`, the draft in the RFC editor queue), so the webmail and the
apps can show and edit them: calendars with their colour, visibility and default, events with
title, place, time zone, all-day and repeats, and a query that expands a series into its instances
for a month or a week. One instance can be moved, renamed or taken out of its series; that becomes
an override or an exclusion the way iCalendar has it. See
[docs/jmap-calendars.md](docs/jmap-calendars.md) for what is supported and what is not (sharing,
invitations and server-side reminders are not).

There is no second copy: events stay iCalendar on disk, in the same calendars. What a phone stores
over CalDAV shows up in JMAP's changes and push right away, and what the webmail writes moves the
sync token and the ETag, so the phone fetches it on its next sync. Everything written over JMAP
goes through the same check as a CalDAV PUT, with the same size limit, so a phone can always read
it and store it back; changed instances are written out whole for CalDAV clients. Dates, time
zones, titles and repeats have limits of their own, which the session announces, and expanding
repeats stops after 10 000 occurrences per series or five seconds per query. Accounts without
calendars, like services, don't get the capability. Migration 32 adds whether a calendar is shown
and which one is the default; the first calendar of every account becomes its default.

**Mail rules on the server.** Everyone can have their mail sorted as it arrives — into folders,
marked as read or flagged, passed on, or thrown away — and it happens on the server, so the rules
hold for every app and while every device is off. The rules are standard Sieve scripts, one of them
active per account. The webmail and the apps are getting a rule editor that writes them; anything
else that speaks Sieve can manage them too: JMAP clients through `urn:ietf:params:jmap:sieve`
(RFC 9661), and Thunderbird's Sieve add-on, Roundcube or `sieve-connect` through ManageSieve on
port 4190 (RFC 5804). See [docs/sieve.md](docs/sieve.md).

A script can file by folder path or by JMAP mailbox id, make a missing folder, set flags and
keywords, discard, and test headers, addresses, the envelope, the body and the size, with
variables and numeric comparisons. What it asks for is checked when it is stored, against exactly
what delivery carries out: `vacation`, `reject`, `regex` and the like are refused then, with the
line that asked, instead of failing quietly on the first mail. Junk stays junk — the spam filter and
the sender lists decide first, and rules only see the mail you want. A script that fails, runs too
long or points at a folder that does not exist leaves the message in the inbox.

A rule can pass a message on only where forwarding could: to people on this server, or to an
address elsewhere that confirmed it through the forwarding link, once per message, with SRS and the
same loop protection. A rule can't turn the server into a mail cannon, and a redirect that is not
allowed keeps the message here instead of losing it.

ManageSieve wants STARTTLS before it offers a login, and takes the same passwords, app passwords
and lockouts as IMAP; the account's IMAP switch covers it. It listens on 4190, set by
`listen.managesieve` and `UWUMAIL_MANAGESIEVE_BIND`, and the installer checks that port like the
others. On a machine where something else already holds 4190 — a Dovecot next door, say —
`update.sh` moves UwUMail's to the next free port rather than failing to start. It is not carried
through the UwUMail Gateway yet; the webmail and the apps manage rules over JMAP and don't need it.

**Reviewed before release.** The new calendars, rules and ManageSieve had their own security
review ([docs/security-audit-0.7.0.md](docs/security-audit-0.7.0.md)). What it found is fixed in
this release: ManageSieve no longer lets a connection pile up memory or stay open without logging
in, a script that runs away no longer holds up delivery, a redirect that reaches nobody keeps the
message, one mail can't make a pile of folders, and calendar requests and blob reads have a budget
of their own. Event ends across a daylight-saving change are now calculated on the clock, not the
calendar. The webmail's review of the same features is in its docs/security-audit-2026-09.md.

## 0.6.3

**The webmail catches up with the app** (webmail [v0.6.3](https://github.com/MinifyX/UwUMail-Webmail/releases/tag/v0.6.3)).
↑ and ↓ open the previous and next mail, also while reading one; Shift extends the selection and
Ctrl+A selects everything loaded in the list. Every row has a spam button on hover, and spam can be
a swipe on the phone. The full addresses of a message — from, reply-to, to, cc, bcc — fold out
under the header and show on hover. A link from a mail no longer opens straight away: a question
shows its full address first, with its real domain in bold, a warning when it isn't what the text
claims, and where a tracking or redirect link leads, worked out without ever loading it. Domains can
be remembered, and the question can be switched off under *Lesen*, except for disguised links,
which always ask. Hovering a link shows where it goes at the bottom of the mail. Sending can be
undone for a few seconds, and signatures are back. The webmail had its own security review
(W-12 to W-21) — see its docs/security-audit-2026-09.md.

The portal takes spam as a swipe action, too.

**The webmail and the apps keep their settings in step.** A new JMAP extension,
`urn:uwumail:jmap:settings`, holds one settings document per account: theme, tone, language, how
mail is shown, the undo window for sending, trusted senders, remembered link domains, per-sender
appearance and signatures. Every device reads it at start and hears about changes over push, so a
signature written on the PC is there in the webmail and on the phone, and a sender trusted on one is
trusted on all. Lists are kept one key per entry, so two devices adding at the same time never
overwrite each other. See [docs/jmap-settings.md](docs/jmap-settings.md).

Everything in it is on a whitelist with rules for its value and limits for its size, because what
one device writes is handed to all the others. Theme, tone, language and the webmail's mail choices
are not stored a second time: they are the portal's preferences, so a change in the portal shows up
in the apps too, and the other way round. One write may name at most as many keys as an account may
keep, removals included, so a single request can't keep the database busy with thousands of them.

The portal had been turning away the webmail's own mail preferences — conversations, density,
remote images, mail appearance, sender pictures and swiping — as unknown, so they never reached the
server and every new browser started from the defaults. It takes them now.

## 0.6.2

**A fetched mailbox brings the mail that was already in it.** Fetching started where the folders
stood on the first run, so everything that was in the mailbox before — often years of it — stayed at
the provider for good, and the only way over was a command on the server. Now it comes when it is
asked for: *Vorhandene Mails übernehmen* when a mailbox is added, on by default, or later with the
clock button in the mailbox's row.

It comes the way the migration import copies a mailbox rather than the way new mail arrives: with
the date it had at the provider and read or unread as it was there — not as a heap of new mail from
today — and filed where the provider had it, the inbox into the inbox and the junk folder into Junk,
without the spam filter judging months-old mail again against DKIM keys the senders have long
rotated. What is already here, by whatever way it came, is recognised and not brought twice, so
asking for it on a mailbox that has been fetching for months is safe. Every run takes 200 messages
of it per folder next to the new mail, and the next portion follows half a minute later until it is
all here; a full mailbox makes it wait at the provider the way it makes new mail wait. Afterwards it
is marked as read or deleted there like any other message.

The dialog's hint about deleting at the provider said only mail that arrived is ever deleted; since
0.6.1 mail the filter refuses is too, and it now says so. A message a provider names in its search
and then does not hand out is now written to the log instead of being passed over in silence.

## 0.6.1

**A mailbox somewhere else needs an address and a password, nothing more.** Setting one up meant
knowing what the provider calls its IMAP server, which port it listens on, and whether it wants the
whole address as the login or only the part before the `@`. Almost nobody knows that about their own
free mail account, and the page guessed it from a short list in its own code — which got iCloud
right and everything else by luck.

The server now works it out. It asks the domain's own SRV records first, then the autoconfig file
the provider publishes, then Mozilla's collection for the providers that publish nothing, and only
then guesses the usual names. mail.de and iCloud answer at the very first step, GMX, web.de and
t-online at the third.

Nothing is stored on the strength of a guess: the server **logs in for real** before the mailbox is
kept, so what is saved is what a connection answered to. That also settles the one thing no source
states reliably — iCloud and web.de want the part before the `@` on the way in, and the whole
address on the way out. A wrong password ends the search where it is, instead of trying the next
candidate with it and filling the provider's lockout counter. The outgoing server is proven
separately and left out when it does not answer, and the dialog says so rather than quietly
dropping the switch.

The choices that are really somebody's own stay where they were, in front: what happens at the
provider afterwards — mark as read or delete for good — how often, whether the junk folder comes
too, and whether this address answers its own mail. The server names only come out for whoever wants
to type them, and by themselves when no provider answered, so a mailbox at a provider nobody has
heard of can still be set up by hand.

Two things this reaches out for: the provider's own file, and Mozilla's collection, which learns the
domain of the address being set up. Both go through the same door as the subscribed word lists —
HTTPS, valid certificates, public addresses only — and so do the logins.

**A fetched mailbox no longer keeps what this server refused.** Mail the filter turned away — a
virus, a blocked sender, a DMARC policy that rejects, a score over the limit — was left untouched
at the provider, so that nothing was ever destroyed there. In practice that meant a free mail
account filling up with exactly the mail this server had already thrown out, run after run, with
nobody emptying it. Refused mail is now dealt with at the provider like mail that arrived: marked
as read, or deleted, by what the mailbox is set to. What was refused and why stays in the history
under Spam filter; the message itself is gone for good only where the mailbox is set to delete.

This deliberately turns back part of the fix for **S-8** in
[docs/security-audit-0.5.2.md](docs/security-audit-0.5.2.md), which had made refused mail stay at
the provider. What S-8 was really afraid of still cannot happen: an answer of "later" —
greylisting — leaves the message where it is, and so does mail this server has nowhere to put,
because a mailbox it fetches into is gone or takes no mail. That one is a mistake on this side, not
a verdict on the message, and somebody's mail is not deleted over it.

**A full mailbox no longer costs mail.** It used to count as a refusal, so everything that arrived
at the provider while there was no room here was stepped past for good — making room afterwards
brought none of it back. It now counts as "later", the way it is answered at the door: the folder
waits, and the mail comes as soon as it fits. Without this, the change above would have gone
further and deleted it at the provider.

## 0.6.0

**The log goes to Grafana Loki, and the gateway's comes along.** Under *Server → Logs* the server
can now send its log lines to a Grafana Loki by itself — your own log server or Grafana Cloud, with
no login, a username and password, or a token — so nothing like Alloy or Promtail has to run next
to it. Lines go out in batches, wait in memory while Loki is away, and never hold up mail; the
panel shows what was sent, what waits, and what went wrong. *Send a test line* tries an address
before anything is saved. Every line carries `app`, `instance`, `source` and `level` labels and is
the same JSON the server writes with `log.format = "json"`, so one set of queries fits both ways
in. Log lines contain login names and IP addresses, so switching this on asks the admin to agree to
sending exactly that off the server; without that agreement the server refuses to switch it on,
from the panel, the terminal and the config file alike.

The UwUMail Gateway now hands its own log to the server through the tunnel. Its lines show up on
the portal's *Logs* page marked *Gateway* and go on to Loki with `source=gateway`, so the VPS
nobody logs into says what it has to say where people look. While the server is away the gateway
keeps its last 1000 lines for it. Both sides only do this when both are new enough: an older
server never asks, and an older gateway never sends.

## 0.5.2

**A security release.** The sixth review looked at everything 0.5.0 added — above all the fetch
feature (pulling mail from other mailboxes and sending as the fetched address) and the spam traps —
at the gateway and tunnel, and re-read the rest of the server; the whole webmail was read alongside.
Details in [docs/security-audit-0.5.2.md](docs/security-audit-0.5.2.md) and, for the webmail, in its
own repository. Every Critical, High and Medium finding is fixed here, each with a regression test.

The one that mattered most: **outgoing mail was routed by the envelope address alone**, so a user
who registered a fetched mailbox for someone else's address could intercept that person's outbound
mail or send as them. Routing and send-as ownership are now scoped to the account, a fetched
mailbox must be a real one elsewhere (not a hosted address), and sending from it needs one
successful fetch first. Alongside it: **DMARC could be bypassed** on the receive path with a second
`From` header, a malformed header line, or a HELO address literal behind a trusted relay — all
refused now; fetched mail no longer trusts a sender-written `Authentication-Results` or a private
`client-ip`; the session cookie is read per transport again; fetched mail is no longer lost or
destroyed at the provider when it is refused, held or the account is trashed; the fetch and send-as
workers only reach public hosts; the SMTP "sending" switch holds on the JMAP door too; and a spam
trap no longer shields its co-recipients. The gateway expires an unused pairing code, stops trusting
a migrated address, and its root helpers no longer follow planted symlinks or run gateway-chosen
commands; the release now gates on `cargo audit` and checks the version against the tag.

The webmail reveals a hidden Bcc, cleans pasted content, keeps a mail's CSS out of the printed
header, does not auto-load remote images for junked mail, names the address an unsubscribe sends to,
and flags more disguised links and dangerous files. It is pinned to its 2026-09 security commit.

## 0.5.1

**The webmail answers on its own address again.** `/mail` worked and so did every path below it,
but `/mail/` — with the slash, which is the address the webmail is built with and therefore the one
a bookmark holds — answered with a not-found. One route was missing between the exact path and the
wildcard below it, because a wildcard wants at least one character after the slash. Found by trying
the release on the test machine before it went anywhere else.

The webmail itself is unchanged: this builds the same commit `webmail.pin` named for 0.5.0.

## 0.5.0

**Security.** Everything new here was reviewed afterwards, and the webmail's own repository with
it, which nobody had read before: [docs/security-audit-0.5.0.md](docs/security-audit-0.5.0.md).
Nine findings, all fixed before this release.

**A mailbox in the browser, under `/mail`.** Getting to your own mail away from your own machine
meant the app: on a borrowed computer, at work or on somebody else's phone there was simply no way
in. The server now brings a mailbox for the browser with it.

It is the UwUMail app's interface, cut to what a browser is good at: reading, writing, folders,
search, conversations, attachments with a preview, reporting spam, blocking a sender,
unsubscribing from a newsletter, keyboard shortcuts — and on a phone the same view as the app,
swipes and all. It can go on the home screen too.

Whoever is signed in to the portal is signed in here. No second password, no second login:
two-factor, passkeys, the session list and locking someone out hold here just as well, because it
is the same session.

Message HTML never reaches the browser unfiltered. The server cleans it by the same rules as the
app, the webmail cleans it a second time, and what is left is shown in a frame without scripts
that loads nothing from other servers until somebody asks it to.

Whoever does not want any of it switches it off: once for the whole server under Settings, and per
account under People. Mail programs are never affected either way.

Not there yet, and meant for the next version: undo send, signatures, sender pictures and address
suggestions. Unsubscribing opens the sender's own page instead of taking the one-click route —
that one would have meant the server calling on an address a mail header named.

The webmail lives in its own repository, [UwUMail-Webmail](https://github.com/MinifyX/UwUMail-Webmail),
and the image is built from the commit `webmail.pin` names: `02bd99d` for this release.

**Greylisting keeps the mail now instead of throwing it away.** When a message looks suspicious,
the server asks the sending server to come back later — real mail servers do, a few minutes on,
and spammers mostly never. That filters well and feels terrible: the mail somebody is waiting for
sits nowhere for a quarter of an hour, and the one that never comes back leaves no trace at all.

The message is now kept while its sender is being asked to come back. Under Spam filter there is a
second tab with whatever is being held right now, and the number beside it says so without opening
it. Every entry shows the sender and the subject and nothing else: whoever wants to read a waiting
message delivers it to their own mailbox first, where a mail program does the usual about pictures
and links. A settings page is the wrong place to show a message the filter has just called
suspicious.

Three ways out, and all three are about this one message: deliver it, throw it away, or throw it
away and let the spam filter learn from it. Throwing away asks first, because the sender's second
attempt will not bring the message back — a row somebody decided about stays behind as a note to
self and keeps the retry from delivering twice or from undoing a discard. Letting a sender through
for good stays an entry in the sender list: the moment the filter has just called a message
suspicious is the wrong one to take its sender off the check for ever, not least because the
envelope address that would land there can be forged.

Delivery itself is unchanged: left alone, the mail still arrives when its sender comes back. Kept
for two days, at most 5 MB per message and 200 messages per person; past that it is greylisted the
way it always was. No admin route leads to these rows — they hold whole messages, and a message
belongs to the person it was addressed to. Off with `spam.greylist_hold`, which also clears out
what is already being kept.

**Every port can move, and the installer asks before it starts.** The mail ports were nailed to
25, 465, 587 and 993, so a machine that already ran something on one of them needed an edited
`compose.yaml` — and an edited `compose.yaml` is what `update.sh` has to stop and ask about. They
now read the same `.env` variables the web ports always have: `UWUMAIL_SMTP_BIND`,
`UWUMAIL_SUBMISSIONS_BIND`, `UWUMAIL_SUBMISSION_BIND` and `UWUMAIL_IMAPS_BIND`, a port or an
`address:port` each.

Only where UwUMail listens moves. From the outside the numbers stay what they are, because other
mail servers only ever try 25 and mail apps expect 465, 587 and 993, so whatever sits in front
sends them on — one field in a router's port forwarding — and behind a gateway the question does
not come up at all.

`install.sh` looks at all six before it writes anything, instead of letting the start fail at the
end on a machine that already looks installed. Every taken port becomes a question with the first
free port as its suggestion, and the answer goes into `.env`. With `--yes` or without a terminal
it stops and names each one with its flag (`--https-bind 8443` and the rest): quietly moving a
mail server's port 25 means mail that never arrives, and that is worse than an installer that did
not run. `update.sh` lifts all six out of a hand-edited `compose.yaml` now instead of two.

**Installing, in four ways from beginning to end.** [docs/install.md](docs/install.md) walks
through a machine of its own and a machine that already runs other containers, each with and
without a gateway, instead of asking the reader to pick the right box at every step. With the
part nobody had written down: which ports have to arrive, how to forward them on a FRITZ!Box, a
Telekom Speedport, a UniFi gateway or an OPNsense, what IPv6 needs instead, and when forwarding
cannot work at all — DS-Lite, a blocked port 25, reverse DNS the provider made up — which is the
moment to put a gateway in front.

**Three smaller things in the installer and the updater.** Every line they set rewrote the `.env`
through a copy next to it, and that copy was made with whatever umask the shell had — 0644 on a
stock Ubuntu, in a directory every user may read, holding what the `.env` holds. Both make it with
0600 now, like the file they replace, and both clear it away when a run ends early. A port is
checked for being a port: `70000` had the right shape, went into the `.env`, and let the start fail
at the end anyway, which is the one thing looking at the ports first is meant to prevent. And a
pairing code is held to the characters a pairing code has, so a line break in one cannot put a
second setting into the `.env`.

## 0.4.0

**Security.** Everything new here was reviewed afterwards, together with every way into the
server: [docs/security-audit-0.4.0.md](docs/security-audit-0.4.0.md).

**One script to set it up, one to update it.** A new machine now needs two lines:

```bash
curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/install.sh
sudo bash install.sh
```

It asks for the host name and a few other things — every answer is a flag too — writes
`/opt/uwumail`, brings the virus scanner along unless the machine is too small for it or you say
no, installs the helper for system updates, starts the server and shows the one-time code.

Later on, `cd /opt/uwumail && sudo bash update.sh`. A server that is already running gets the
script once with
`curl -fsSLO https://github.com/MinifyX/UwUMail-Server/releases/latest/download/update.sh` in its
directory, and from then on the script keeps itself up to date. It fetches a newer `update.sh` and hands over
to it, backs up, brings `compose.yaml` up to date, pulls and waits for the server's own health
check — and when that does not answer, puts the version from before back and says so. A
`compose.yaml` you edited is not walked over: what fits goes into `.env` (a moved web port, a
pinned tag, the virus scanner), and anything else stops the update with a diff and `--force`.
Both scripts check what they download against a `sha256` published next to it.

**The update button is gone from the portal.** *Server → Updates* still says what is new and what
changed; the machine does the update, with `update.sh`. The helper beside the server now only
does what only root can: install the system's updates and restart the machine. It no longer takes
a version number from the container, so a job is one verb and nothing else.

**Accounts instead of people, and mailboxes for programs.** *Server → Personen* is *Server →
Konten* / *Accounts*, with filters for people, services and a single domain. A **service** is a
mailbox that belongs to a program: it never signs in to the portal, has no password of its own,
and gets in with app passwords an admin makes on its page. A person becomes a service and back
with one button; the mail stays, and the password they had lives on as an app password that does
not expire.

Every account has five switches — SMTP, IMAP, JMAP, calendars, contacts. A switch that is off
holds every password at the door, whatever the password says it may do, because the check happens
at the login. With neither IMAP nor JMAP an account has no mailbox at all: mail to it is refused
at the door, or handed to the one address you name instead. A sender that only sends now costs
nothing and fills nothing up.

App passwords follow the same switches: the page that makes one only offers uses the account
actually has, and one whose every use is switched off is refused instead of handed out.

**The change log opens.** Every entry folds out to who did it, what it was about, when, from
where, and the details exactly as they are stored. Twenty actions that used to read as their raw
name now have a sentence of their own.

**One panel instead of two views.** The Simple/Pro switch is gone and everything is simply there.
A calmer view for people who only want the traffic light may come back later, more deliberately.
For an admin the two menu groups fold away instead, and the browser remembers which.

**Settings from the terminal.** `uwumail-server settings list|get|set|unset` reaches the same
settings as the panel, with the same checks and the same order — which is how the installer
switches the scanner on before there is a portal to log into. Secrets are written with `-` and
read from standard input, so they stay out of the shell history.

**Fixed.** On the gateway, the helper that carries out what the portal asks for kept starting
itself instead of doing the work: the guard that tells the copy from the original had lost its
variable, so no gateway job ever ran. An app password whose every use is switched off is refused
now instead of being handed out and never opening anything.

## 0.3.0

**Security.** Both of the additions below were reviewed afterwards:
[docs/security-audit-0.3.0.md](docs/security-audit-0.3.0.md). Two small findings, both fixed.

**A virus scanner, if you want one.** ClamAV can now look at every message before it is taken.
It runs in its own container beside the server — clamd wants two gigabytes of memory and a
writable place for its signatures, which a read-only image on a Raspberry Pi does not have — and
it stays out of the way until you ask for it: `docker compose --profile antivirus up -d`, then
switch it on under *Spam filter → Viruses*.

A find means the message is never accepted: the sending server gets a `554` and tells its own
sender, and the find is written into the spam history with its name. Our own people are checked
the same way, so nothing infected leaves the house either. A scanner that is away never stops the
post: the message goes on and carries `X-Virus-Scanned: no (…)`, the server log says why and the
health overview turns red. The new page shows the version, the age of the signatures and what was
turned away in the last thirty days, and sends the harmless EICAR test file on a button press.
The whole story: [docs/antivirus.md](docs/antivirus.md).

**DNS records at Cloudflare.** TXT values now go there in quotes, and split into several strings
once they outgrow the 255 bytes one string may hold — the way Cloudflare's own dashboard writes
them, so it stops marking our records as unquoted. A record that is already right but sits there
without quotes gets them on the next run, which changes nothing about what DNS answers.

A record that works but does not read the way UwUMail would write it — a DMARC policy with other
tags, TLS reports going to another address — still counts as fine. It now says so in the DNS
check, and the Cloudflare button offers to bring it into our wording under its own heading,
unticked. MX and SPF carry a warning there: rewriting them means exactly our value, so another
sender or a second MX would fall away.

## 0.2.3

**Security.** Two of these are reachable from the internet without a login, and both stop the mail
until the server is restarted. If you run UwUMail, this is the release to take.

- **A search could end the whole server.** `(`, `NOT` and `OR` each make the IMAP search parser
  call itself, and nothing counted the levels — while a command line may be 64 KiB, which is far
  more nesting than any stack holds. A stack overflow cannot be caught: it takes SMTP, IMAP, JMAP,
  the portal and the queue with it, and the next line takes the restarted one again. The parser now
  refuses anything nested deeper than real clients ever go.
- **A stranger could make the server set aside a message worth of memory per connection.** `APPEND`
  may carry a whole message, and that much was reserved the moment a literal was *announced* —
  before the bytes arrived and before anyone had logged in. The generous limit now belongs to people
  who are logged in.
- A message far too big to be a report is no longer unpacked as one, an announced literal length can
  no longer wrap, and the helpers that run as root now check what they are handed on both sides and
  refuse to install an older version than the one running.
- The whole stack was reviewed, the desktop client for the first time:
  [docs/security-audit-2026-09-18.md](docs/security-audit-2026-09-18.md). Nine findings, all fixed.
  The two above were found by a new randomized parser test that runs in CI
  (`crates/uwumail-imap/tests/robustness.rs`).

**Updates from the portal.** With a small helper installed beside the container
([docs/install.md](docs/install.md#buttons-instead-of-commands-optional)), *Server → Updates* now
does it instead of showing a command:

- **Update now**, or on a day and time you choose. It backs up first, and a failed backup means
  nothing is touched — with one deliberate way past that for a server with nowhere to back up to.
- A scheduled update keeps out of the backup's way: not in the half hour before one, not while it
  runs, not in the half hour after. When its minute falls inside that window it waits and tries
  again, for up to six hours.
- The update replaces the container that asked for it, so the page keeps knocking through the gap
  and the result is there when the server comes back.
- If the new version does not answer its health check, the tag from before goes back.

**The machine, and the gateway's.** The portal can install the system's updates and restart either
machine. On the mail server's own machine it says plainly that something else may be running there
and that this is nobody's responsibility but yours. On the VPS it can also fetch and install a newer
gateway. Nothing but a word from a fixed list and a version number ever crosses over; the addresses
and the checksums come from the helper's own constants.

**Restoring a backup.** From the portal, beside the snapshot, or from the setup assistant on a
machine that has no server yet — which is what you want when it stands in for one that died.
Afterwards backups are switched off (the snapshot carries the old server's target), this machine's
gateway pairing is kept, and the database from before is kept beside the new one.

> The backup and restore functions in this release are **untested in practice**. The code is there,
> the unit tests pass and the refusal path was checked on a real machine — but no snapshot has been
> fetched from a real backup server and put back yet. Do not rely on it as your only way back.

**Reports.** *Server → Reports* shows what other servers report about your domains: DMARC and TLS,
who reported, how much passed, which connections failed, and a curve over time. What a report says
is kept, not only how much it counted.

**Spam history.** A second tab on the spam page shows what the filter decided for every message and
why. The subject of spam is always kept; the subject of clean mail stays hidden until you ask for
it.

**On a phone.** The portal no longer scrolls sideways. That was two things: grid and flex children
default to a minimum width of their content, and buttons refused to wrap. Both are fixed
everywhere, not page by page.

**Smaller things**

- *Server → Settings → Sending* says when mail leaves through a UwUMail Gateway, so nobody changes
  something there that the gateway decides.
- The gateway installer and the setup assistant say that the VPS belongs to the gateway alone.
- Backups remember when a run started and finished, and can start on a minute rather than an hour.
- A restore checks a snapshot before writing it and carries on after a connection breaks.
- A report with a made-up date can no longer crowd out the real ones.

## 0.2.2

- **The installer never actually switched the firewall on, and then said it had.** `ufw status`
  answers `Status: inactive` when it is off, and the check looked for "active" without anchoring
  it — which matches "in-active" just as happily. So the installer believed ufw was already
  running, skipped switching it on, stood the old nftables rules down, and printed a summary
  saying the firewall was up. A gateway updated with 0.2.0 or 0.2.1 that had the handwritten
  nftables rules is left with **no firewall at all**. Update to this version, or switch it on by
  hand with `ufw --force enable`; `sudo bash install.sh --check` now says truthfully which of the
  two it is.
- Same mistake in the report the portal reads: a firewall that was off was shown as active.
- The order is safer as well now. ufw goes on before the old rules come down, so there is no moment
  without a firewall, and ufw is told to load its rules again afterwards — stopping nftables runs
  `nft flush ruleset`, which empties the table for everyone, and systemd does not always finish
  that before the next command runs. The run ends with one last check that the firewall is really
  up, and says so loudly if it is not.
- The check for the SSH rule asks `ufw show added` instead of `ufw status`, which prints no rules
  at all while ufw is off — that is exactly when the check matters, right before switching it on.

## 0.2.1

- The installer recognises the gateway's own handwritten firewall rules in both wordings it went
  out in, not just the one from `docs/gateway.md`. On a machine with the other one, 0.2.0 switched
  ufw on and left nftables running beside it: two firewalls with their own idea of what is open,
  which is a bad thing to go looking for later. Rules that are not the gateway's are still left
  alone and only reported.

## 0.2.0

- The UwUMail Gateway looks after the machine it runs on. The same install command as always does
  it — a first install, an update, and a check that what was set up is still there — and it now
  sets up ufw with the ports the gateway needs and the port SSH really listens on, fail2ban against
  SSH guessing, and unattended-upgrades for security updates only, never rebooting on its own. It
  reports what it found instead of changing things quietly, and files you edited afterwards are
  left alone: the new version lands beside yours as `.new`. `--no-harden` skips all of it,
  `--check` changes nothing and only reports. See `docs/gateway.md`, "What it does to the machine".
- Updates on the gateway show up in the portal under *Server → Setup*: how many wait, how many are
  security updates, whether a restart is due, whether a newer system version is out — with the
  whole SSH command to install them, ready to paste. The same thing greets you when you log into
  the gateway over SSH. Nobody logs into a VPS for weeks, so it says so where it is noticed.
- Nothing on the gateway can lock your server out. Its address changes every night, and behind
  carrier-grade NAT the neighbours share it, so the address a stranger brute-forces SSH from today
  can be the one your server connects from tomorrow. Every ban is TCP only while the tunnel is QUIC
  over UDP, so a ban cannot touch it; fail2ban asks before each ban and is told where the tunnel
  comes from; and a timer frees an address that was banned before your server moved onto it. When
  your server moves off an address, the gateway stops vouching for it — last night's address
  belongs to the next customer by morning. IPv6 counts as the whole /64 that one connection is
  handed, IPv4 as the single address.
- Guessing at mailbox names is stopped sooner than guessing at passwords: three tries at logins
  that do not exist here, instead of ten. Whoever works through `info@`, `sales@` and `admin@` is
  reading the address book, not getting close to a password. Three and not one, because at one try
  the block itself would answer "does this mailbox exist?"; what the other side is told stays word
  for word the same either way, and the password check still runs against nothing, so the clock
  gives nothing away. A network that is turned away is handed to the gateway, which keeps it off
  its public ports for an hour — only the server can see a failed login, since the gateway carries
  TLS it cannot read.
- Port 25 has no ban list on purpose, and gets none: the gateway cannot see who fails to log in
  there, so a jail could only count connections, and banning a mail server for connecting often
  means losing its mail.
- The handwritten nftables rules this page used to hand out are stood down by the installer when it
  finds them, and kept as `/etc/nftables.conf.before-uwumail-ufw`. Any other rule set is left alone
  and reported instead.

## 0.1.2

- UwUMail Gateway: `uwumail-server gateway pair <code>` pairs from the command line, for when the
  portal cannot be reached and the code should not go into the configuration. It takes effect after
  a restart. A pairing code that the gateway shows again with other addresses or another port is
  taken over while the gateway has not accepted the pairing yet; before, the server kept the
  addresses that never worked.
- Gateway on the VPS: `uwumail-gateway code`, `unpair` and `check-config` read
  `/etc/uwumail-gateway/gateway.toml` by themselves. Before, only the service did: with
  `public_addresses`, `tunnel` or `state_dir` set, the pairing code carried the wrong addresses or
  port, and `check-config` checked the defaults instead of the file. Update the gateway for this,
  with the same commands that installed it (`docs/gateway.md`, "Install the gateway").
- Certificate: after a failed order the server asks Let's Encrypt which names it refused. When its
  own name is among them, as while the tunnel to the gateway is down, it leaves none out and tries
  again in an hour; otherwise it leaves the refused ones out for a day. Before, any failure left
  every extra name (`imap.`, `autoconfig.`, `mta-sts.` and the like) off the certificate for a day.
  That still happens when Let's Encrypt names none, for example when the order fails before any
  name is checked.
- Forgetting a gateway: the portal and the command line said the server pairs again by itself when
  the code stays in the configuration. It tries with a new key, the gateway refuses that, and mail
  to other servers waits in the queue. The texts say so now, and what to do instead.
- Portal: an app password and the recovery codes stay on screen when you click beside the window,
  and Escape or the X asks first. They are shown once and nowhere else.
- `deploy/next-to-mailserver`: `UWUMAIL_PROXY_BIND` in `.env`, for example `127.0.0.1:8080`, binds
  the plain HTTP port 8080 to one address, so only the reverse proxy reaches it. The line belongs
  to that folder's `compose.yaml`, not to the server: an installation from before this version
  takes the current `compose.yaml` first, otherwise it does nothing.

## 0.1.1

- Apple configuration profiles: an iPhone ended up with an empty file and refused it as an invalid
  profile. Safari asks for the link twice — once to download the profile, once to install it — and
  the link burned on the first request. It now works until its ten minutes are up, and it arrives
  as a profile instead of as a download.
- Installing next to another web server: `UWUMAIL_HTTP_BIND` and `UWUMAIL_HTTPS_BIND` in `.env`
  move UwUMail's web ports, and `deploy/behind-proxy` has ready files for Caddy and other reverse
  proxies. A reverse proxy that reaches port 80 gets a page saying what to change instead of a
  redirect loop, the log names once per start a proxy that is missing from `http.trusted_proxies`,
  and `check-config` checks that list.
- UwUMail Gateway: the pairing code goes into `.env` as `UWUMAIL_GATEWAY_CODE` before the first
  start, so the setup assistant is reachable through the gateway right away; `docs/install.md`
  follows that order now. The certificate is ordered as soon as the tunnel is up, instead of up to
  an hour later.
- Portal: a form dialog stays open when you click beside it, and Escape or the X asks before
  throwing away what you typed.
- The new `.env` lines belong to `compose.yaml`, not to the server: an installation from before
  this version loads the current `compose.yaml` first, otherwise they do nothing.

## 0.1.0

The first version I use instead of mailcow.

- Mail: SMTP with DKIM, SPF, DMARC, MTA-STS and TLS reports; JMAP; IMAP on port 993 with IDLE,
  CONDSTORE and QRESYNC; calendars and contacts over CalDAV and CardDAV.
- Mail apps: autoconfig, Autodiscover and Apple configuration profiles with their own app password.
- People and domains: aliases, sub-addresses, catch-all, forwarding with confirmation, forwarding
  addresses without a mailbox, sending as a whole domain, app passwords, authenticator apps and
  passkeys.
- Spam filter: rules, reputation, Bayes, greylisting, sender lists with patterns, word lists,
  built-in lists and limits per person.
- UwUMail Gateway: a VPS in front of a server at home, over a QUIC tunnel.
- Moving from mailcow: export script, import of people with their password hashes, aliases,
  settings, DKIM keys, calendars and contacts, and copying mail over IMAP;
  `uwumail-server account admin` names the admin afterwards.
- Backups: nightly to SFTP, deduplicated and encrypted, with restore from the command line.
- Updates: the portal shows new versions of the chosen channel and the commands to update.
- Installing: a step-by-step guide in `docs/install.md`, images tagged `latest`, and a ready
  gateway for amd64 with every release.
