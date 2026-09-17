# Changelog

Each release gets a section here before its tag is pushed; CI copies the section into the GitHub
release. Versions follow semver; `-beta.N` versions are pre-releases.

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
