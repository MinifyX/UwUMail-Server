# Changelog

Each release gets a section here before its tag is pushed; CI copies the section into the GitHub
release. Versions follow semver; `-beta.N` versions are pre-releases.

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
