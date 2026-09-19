# Security review — what 0.4.0 adds, and every way in, 19 September 2026

The fourth pass, after [security-audit.md](security-audit.md),
[security-audit-2026-09.md](security-audit-2026-09.md),
[security-audit-2026-09-18.md](security-audit-2026-09-18.md) and
[security-audit-0.3.0.md](security-audit-0.3.0.md). Two things were looked at:

- **everything 0.4.0 adds**: service accounts and the switch per protocol, app passwords an admin
  makes for a service, the change log that opens up, `install.sh` and `update.sh`, the settings
  command line, the host helper losing the update path;
- **every way into the server**, once more and on purpose: the portal login with its second
  factors and passkeys, IMAP, JMAP, SMTP submission, CalDAV and CardDAV, app passwords, password
  links, the setup code, and what the helpers beside the server may ask for.

Done with Claude, not an independent firm. An honest sweep, not a certificate. Nothing was run
against the production server; the live checks happened on the test instance.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 0 | 0 | 0 |
| High | 0 | 0 | 0 |
| Medium | 2 | 2 | 0 |
| Low | 4 | 4 | 0 |

Six findings, all fixed before the release. Six more things are deliberate and written down under
[Accepted](#accepted) so they are not rediscovered as findings later.

## Findings

### F-1 · Medium · Mail for a service without a mailbox was still delivered into one

`crates/uwumail-smtp/src/submission.rs`, `dsn.rs`, `vacation.rs`, `forward.rs`,
`crates/uwumail-web/src/notices.rs`

An account with neither IMAP nor JMAP has no mailbox: mail to it is refused at the door with a
`550`, or handed to the one address an admin named for it. That was true at RCPT time on the way in
from other servers — and nowhere else. Every other way a message reaches a mailbox looks the
recipient up again for itself, and each of them delivered into whatever account it found:

- a message from a mail app on this server (submission, the common case),
- a bounce addressed to the service,
- a vacation reply,
- a forward from a forwarding address that points at the service,
- a security notice the portal writes.

Found by trying it on the test instance, not by reading: a message an authenticated sender wrote to
the service landed in the service's own inbox, which is exactly the mailbox that is not supposed to
exist. Nothing leaked outward — the mail stayed on the server, in an account only an admin can open
— but a mailbox nobody reads fills up and, worse, the address that should have received it never
does.

**Fixed** in `619bb42`. `Store::delivery_target` answers, in one place, which account a message for
this one is stored under: itself, the single address it hands its mail to, or nothing at all. All
five callers ask it. A test in `crates/uwumail-smtp/tests/flow.rs` sends to a service from outside
and from a mail app on the server, with and without an address named for it, and checks that
nothing is ever stored under the service.

### F-2 · Medium · A protocol switched off only held for five minutes on JMAP and CalDAV

`crates/uwumail-jmap/src/auth.rs`

JMAP and DAV see the account password on every single request, and checking an Argon2 hash is slow
on purpose, so a correct password is remembered for five minutes. That memory asked whether the
account may log in at all and whether its credentials had changed since — but not whether the
account may still use this protocol. Switching JMAP (or calendars, or contacts) off therefore did
nothing for up to five minutes for a client that had just been there, which is the one moment when
an admin switching it off most wants it to mean something.

Only logins with the account password are remembered; an app password is a fast lookup and goes
through the full check every time.

**Fixed** in `c68b1e6`. The remembered login now asks the same question the login asks, through one
new function, `Account::may_use`, which the store's own gate also calls, so the two cannot drift
apart. `crates/uwumail-jmap/tests/api.rs` switches JMAP off between two requests and expects the
second to be refused; without the fix that test fails.

### F-3 · Low · The `.env` with the gateway code became readable for everyone on the machine

`install.sh`

`install.sh` creates `/opt/uwumail/.env` with mode 0600, because a gateway pairing code lives in
it: whoever has it can pair a server of their own to that gateway. Writing the answers into the
file then built a temporary file next to it and moved it over the original — and a move takes the
temporary file's rights with it, which with root's usual umask means 0644. Every local user could
read the code afterwards. It needs a shell on that machine first, which is why this is low.

**Fixed** in `7862d05`. The new content is copied into the file that is already there, so the file
keeps the rights it was created with. `update.sh` did it that way from the start.

### F-4 · Low · Turning the service switch off took the admin flag off any account

`crates/uwumail-web/src/routes/people.rs`

`PATCH /api/admin/people/{login}` with `service: false` meant "make this a plain user" for every
account, not only for a service. Sent for an admin — which the portal never does, but the API is
the API — it quietly took their admin flag away. Only an admin can call it, and the last admin is
still protected by the store, so this is a footgun rather than a way up.

**Fixed** in `b31127d`. The switch now only ever brings a service back to being a person; the admin
flag is changed by the admin flag alone.

### F-5 · Low · The gateway's helper never carried out a single job

`deploy/gateway/hardening/helper`

The helper on the gateway VPS runs the portal's jobs — install the system's updates, restart the
machine. Because a gateway update overwrites the helper's own file while it runs, it copies itself
first and works from the copy. The guard that tells the copy from the original had lost its
variable somewhere on the way into the file, so the test was always true: every run copied itself
and started again, and the work never happened. The portal's button therefore did nothing on the
gateway, and the security updates it offered were never installed there.

**Fixed** in `7146872`. Found by putting ShellCheck over every script in the repo, which pointed at
the constant comparison.

### F-6 · Low · An app password could be made whose every use is switched off

`crates/uwumail-store/src/security.rs`

A password with only uses the account may not have would never open anything — the login gate holds
it at every protocol — but it was created, shown once, and written down as if it worked. A secret
that exists and does nothing is a secret somebody has to keep for no reason.

**Fixed** in `df3c537`. Creating one is refused when none of its uses is switched on. Rights beyond
the switched-on ones are kept, so switching a protocol on later makes an old password work, which
is the behaviour the tests already pinned down.

## What was checked and held

Every way in, with a service account on the test instance and by reading the code:

| Way in | What holds it |
| --- | --- |
| Portal login, second factor, passkey | `can_use_portal()` on the login, on both second-factor steps and on every request that carries a session cookie; a service is refused with the same answer and the same time spent as a wrong password |
| Password links (invitation, reset) | `can_use_portal()` when the link is made and when it is opened |
| IMAP, JMAP, SMTP submission, CalDAV, CardDAV | one funnel, `authenticate_mail`, which checks the switch before the password on both the app-password and the account-password path; DAV asks a second time, per collection, because one password covers calendars and contacts |
| App passwords | scopes, expiry, and now the switch; a service's are made by an admin on its page and nowhere else |
| The address itself | `delivery_target` decides where mail for an account is stored, for every way a message arrives |
| The helper on the machine | two verbs, `os-update` and `reboot`, and nothing else: no command, no path, no address, no version |

Tried live on the test instance, with a service that may only send: it sent, IMAP refused the same
password (`this account may not use this protocol`), the portal answered `401`, mail to its address
came back `550 … This address does not take mail`, and with an address named for it both a message
from outside and one from a mail app on the server arrived there instead.

`install.sh` and `update.sh` were run against a stand-in release on the test instance: a file whose
checksum does not match is never installed, a newer `update.sh` replaces itself and hands over with
the flags it was given, a `compose.yaml` that was edited by hand is not walked over, and the
version and the e-mail address are checked for the characters they may hold before they land in the
`.env`.

## Accepted

**A-1 · Becoming a service keeps the password it had, as an app password that does not expire.**
That is the point: what already works keeps working. It also means a password that mail apps were
refused — because a second factor was on, or because app passwords were required — opens the mail
protocols afterwards. An admin does this knowingly, the portal says so before the button, and the
second factors go with it because there is nothing left to sign in to.

**A-2 · A switch takes effect at the next login.** An IMAP connection or a JMAP event stream that
is already open is not cut when the switch goes off. Mail apps reconnect constantly, so the window
is short; `disabled` has always behaved the same way.

**A-3 · An admin makes a service's app passwords without confirming their own password again.**
Every other admin action on somebody else's account works that way, including setting their
password. Each one is in the change log with who, when and from where.

**A-4 · The two scripts check what they download against a `.sha256` from the same release.** That
catches a broken or tampered transfer, not a release somebody replaced at the source. Signing them
would need a key people can check, which is a bigger thing than this project has today.

**A-5 · The change log now shows an admin the stored details as they are.** Secrets never get
there: a setting marked as a secret is written as `•••` before it is stored, by the portal and by
the command line alike, and the only new details are names and switches.

**A-6 · UwUMail updates itself only when a person runs `update.sh`.** The portal says what is new
and nothing more. The helper beside the server keeps what only root can do, and no longer takes a
version number from the container, so a server somebody took over can ask for the system's updates
or a restart, and for nothing else.

## What was not looked at

The same as last time: the Rust dependencies beyond `cargo audit` in CI, the container base image
beyond Trivy in CI, and anything about the machines these run on. The portal's React app was read
for what it sends and shows, not audited as a front end.
