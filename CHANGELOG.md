# Changelog

Each release gets a section here before its tag is pushed; CI copies the section into the GitHub
release. Versions follow semver; `-beta.N` versions are pre-releases.

## 0.5.0

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
and the image is built from the commit `webmail.pin` names: `c65be6d` for this release.

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

**Two smaller things in the installer.** Every line it sets rewrote the `.env` through a copy next
to it, and that copy was made with whatever umask the shell had — 0644 on a stock Ubuntu, in a
directory every user may read, holding what the `.env` holds. It is made with 0600 now, like the
file it replaces. And a port is checked for being a port: `70000` had the right shape, went into
the `.env`, and let the start fail at the end anyway, which is the one thing looking at the ports
first is meant to prevent.

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
