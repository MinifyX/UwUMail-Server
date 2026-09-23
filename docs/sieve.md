# Mail rules (Sieve)

Everyone can sort their own incoming mail with rules: into folders, flagged or read, passed on,
or thrown away. The rules are standard [Sieve](https://www.rfc-editor.org/rfc/rfc5228) scripts,
kept per account on the server and run when mail arrives, so they work the same whether the mail
is read in the webmail, the UwUMail apps or any other mail app — and while every device is off.

The webmail and the apps write the script themselves from a rule editor (one script called
`UwUMail`). Anything that speaks Sieve can manage the scripts too:

- **JMAP** — [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661), `urn:ietf:params:jmap:sieve`.
- **ManageSieve** — [RFC 5804](https://www.rfc-editor.org/rfc/rfc5804) on port 4190, for example
  Thunderbird's Sieve add-on, Roundcube's managesieve plugin or `sieve-connect`.

Both see the same scripts; a change in one shows up in the other at once.

## What a script can do

A script may `require` these extensions, and only these — they are exactly the ones delivery
carries out:

| Extension | What for |
| --- | --- |
| `fileinto` | file into a folder by its path, `/` between the levels (`Work/Boss`); `INBOX` means the inbox |
| `mailbox` | `fileinto :create` makes a missing folder, `mailboxexists` asks for one ([RFC 5490](https://www.rfc-editor.org/rfc/rfc5490)) |
| `mailboxid` | `fileinto :mailboxid "m12" "Work/Boss"` files by JMAP mailbox id, so a renamed folder still works; if the id is gone, the name counts ([RFC 9042](https://www.rfc-editor.org/rfc/rfc9042)) |
| `copy` | `fileinto :copy`, `redirect :copy` keep the message where it would have gone as well ([RFC 3894](https://www.rfc-editor.org/rfc/rfc3894)) |
| `imap4flags` | `addflag`, `setflag`, `removeflag`, `hasflag` and `:flags` ([RFC 5232](https://www.rfc-editor.org/rfc/rfc5232)); `\Seen`, `\Flagged`, `\Answered` and `\Draft` become `$seen`, `$flagged`, `$answered` and `$draft`, other flags become keywords of the same name |
| `envelope` | tests on `MAIL FROM` and the recipient the message arrived for |
| `body` | tests on the text of the message ([RFC 5173](https://www.rfc-editor.org/rfc/rfc5173)) |
| `variables` | `set`, `${...}` and match variables ([RFC 5229](https://www.rfc-editor.org/rfc/rfc5229)) |
| `relational` | `:count` and `:value` with `gt`, `ge`, `lt`, `le`, `eq`, `ne` ([RFC 5231](https://www.rfc-editor.org/rfc/rfc5231)) |
| `subaddress` | `:user` and `:detail` for `name+detail@` addresses ([RFC 5233](https://www.rfc-editor.org/rfc/rfc5233)) |
| `comparator-i;ascii-numeric`, `comparator-i;ascii-casemap`, `comparator-i;octet` | the comparators |

Plus everything in the base language: `keep`, `discard`, `redirect`, `stop`, `if`/`elsif`/`else`,
and the tests `address`, `header`, `exists`, `size`, `allof`, `anyof`, `not`, `true` and `false`.

**Not here, on purpose:** `reject` and `ereject` (refusing a message that was already accepted
sends a bounce to whoever the sender claims to be — mostly innocent people), `vacation` (the
vacation reply is its own feature, in the portal and over JMAP as `VacationResponse`), `enotify`,
`include`, `editheader`, `duplicate`, and `regex` (the engine limits each match, but not all of them
together, so one script could keep delivery busy). A script that asks for any of them is refused
when it is stored, with the line that asked.

### Redirect

`redirect` passes the message on through the same path as forwarding (*Weiterleitung* in the portal, and
`smtp.allow_external_forwarding` in the [configuration](configuration.md)), with its
checks and limits:

- to **people on this server** at once;
- to an address **elsewhere** only when it is a confirmed forwarding target of the account (the
  owner of the address clicked the link sent from *Weiterleitung*), external forwarding is
  allowed on the server and not blocked for the account; it goes out with SRS like every forward;
- **one redirect per message** at most, never to the account itself, and not for a message that
  was here before (its `Delivered-To`) or has travelled through more than 25 servers.

A redirect that is not allowed is left out and logged, and the message stays in the inbox, even
without `:copy`: a rule can't make mail disappear by pointing it somewhere it may not go.

A script can't send mail to arbitrary addresses: whatever it names that forwarding would not
reach, it does not reach either. Note that a confirmed external target receives all mail through
forwarding anyway, so a rule for "only these messages to my other address" works for addresses on
this server today; a rules-only confirmation for addresses elsewhere is still to come.

## When the script runs

At final delivery, for every account the message is for — mail from other servers and mail
[fetched from another provider](fetch.md) alike — after the spam filter, the sender lists and
forwarding have had their say:

- **Junk stays junk.** A message the spam filter or a blocked sender sends to Junk goes there
  without the script ever seeing it. Rules are for the mail you want.
- **Forwarding without a copy** passes the message on and keeps nothing, so there is nothing left
  for the script.
- Otherwise the active script decides. Without one, or when it does nothing, the message goes to
  the inbox (the implicit `keep`).

A message filed into several folders with the same flags is stored once, in all of them; with
different flags, once per set of flags. A folder that does not exist and has no `:create` means
the inbox. A discarded message is accepted (the sender hears `250`) and stored nowhere, and gets no
vacation reply.

**Anything going wrong keeps the message in the inbox**: a script that runs too long (20 000
instructions), wants too much memory (4 MB), hands back more than 64 actions, or fails in any other
way. The log says so, with the account's number and the reason, never with anything of the script
or the message.

Scripts run on a few threads of their own (half the processor cores, two to eight), each for at
most ten seconds. The engine counts instructions, not what a single test costs, so a run can go on
after its ten seconds; it keeps its thread until it is done, and meanwhile new mail for that
account goes to the inbox without the script. However many scripts misbehave, they never hold more
than those few threads.

## Limits

| | |
| --- | --- |
| Scripts per account | 16 |
| Size of a script | 64 KiB |
| Length of a name | 512 bytes (128 characters in any script); no control characters |
| Redirects per message | 1 |
| Active scripts | one at a time; the active one can't be deleted before it is switched off |

## JMAP

The capability in the session is `{"implementation": "UwUMail Server"}`; the account's
`accountCapabilities` carry the limits above:

```json
"urn:ietf:params:jmap:sieve": {
  "maxSizeScriptName": 512,
  "maxSizeScript": 65536,
  "maxNumberScripts": 16,
  "maxNumberRedirects": 1,
  "sieveExtensions": ["body", "comparator-i;ascii-casemap", "comparator-i;ascii-numeric",
    "comparator-i;octet", "copy", "envelope", "fileinto", "imap4flags", "mailbox", "mailboxid",
    "relational", "subaddress", "variables"],
  "notificationMethods": null,
  "externalLists": null
}
```

Methods: `SieveScript/get`, `/changes`, `/set`, `/query` (filter `name`, `isActive`, with `AND`,
`OR`, `NOT`; sort by `name` and `isActive`), `/validate`. `/queryChanges` answers
`cannotCalculateChanges`. Push reports `SieveScript`.

The content goes through the normal upload (`POST /jmap/upload/{accountId}/` with
`Content-Type: application/sieve`) and is named by its `blobId` in `SieveScript/set` and
`/validate`. A script's own `blobId` downloads as `application/sieve` from the download URL and can
be the source of another script. The blob id follows the content: after an update with a new
`blobId`, `updated` names the new one.

What the rule editors do to save, in one request after the upload:

```json
["SieveScript/set", {
  "accountId": "a1",
  "create": { "r": { "name": "UwUMail", "blobId": "b…" } },
  "onSuccessActivateScript": "#r"
}, "0"]
```

(or `update` with the existing id and the new `blobId`). Errors are RFC 9661's: `invalidSieve`
with the problem and its line in `description`, `alreadyExists` with `existingId`, `tooLarge`,
`overQuota` for the seventeenth script, `sieveIsActive` for destroying the active one, and
`blobNotFound` for a blob id this account has not uploaded. `isActive` is server-set: it changes only
through `onSuccessActivateScript` and `onSuccessDeactivateScript`, which apply only when every
create, update and destroy of the call succeeded. A name of `null` on create gives the first free
`script-<n>`.

## ManageSieve

On port 4190 (`listen.managesieve` in the [configuration](configuration.md); an empty value switches
it off, `UWUMAIL_MANAGESIEVE_BIND` in `.env` moves it). The connection starts in plain text, and
`AUTHENTICATE "PLAIN"` is offered only after `STARTTLS`; there is no port with TLS from the first
byte.

Logins are the ones IMAP takes: the account password, or an app password with the *Mail* scope,
where the account needs one. Failed logins count towards the same lockout as IMAP, and the account's
IMAP switch covers ManageSieve too. The app password list shows `MANAGESIEVE` as where it was last
used.

Commands: `CAPABILITY`, `STARTTLS`, `AUTHENTICATE`, `LOGOUT`, `NOOP`, `UNAUTHENTICATE`,
`HAVESPACE`, `PUTSCRIPT`, `LISTSCRIPTS`, `SETACTIVE`, `GETSCRIPT`, `DELETESCRIPT`, `RENAMESCRIPT`
and `CHECKSCRIPT`. Literals may be sent as `{n+}` or `{n}`; the server never waits for a
continuation. A command line may be 8 KiB, a literal 4 KiB before logging in and a script's size
after; one command may have at most seven literals, which together may hold 4 KiB before logging
in and a script and its name after. A connection idle for a minute before logging in, or 31 minutes after, is closed.

**Behind a [UwUMail Gateway](gateway.md)** port 4190 is not carried through the tunnel yet: the
tunnel's list of services is read strictly by gateways of today, so a new name in it would break the
pairing with them. Manage the rules over JMAP from outside (the webmail and the apps do), or reach
4190 in your own network.

## How it is built

The Sieve engine is [`sieve-rs`](https://github.com/stalwartlabs/sieve) (AGPL-3.0-only, like this
server), compiled and run in-process. Two of its quirks are straightened out before a script is
compiled, on the first line so line numbers stay the author's: `fileinto :mailboxid` asks it for
the `mailbox` extension instead of `mailboxid`, and a variable first set inside a block would be
forgotten at the end of it, where RFC 5229 keeps it for the whole script. Both are covered by
tests (`crates/uwumail-smtp/src/sieve.rs`).

- `crates/uwumail-store/src/sieve.rs` — the scripts, migration 0033.
- `crates/uwumail-smtp/src/sieve.rs` — checking a script, running it into a plan.
- `crates/uwumail-smtp/src/rules.rs` — carrying out the plan at delivery.
- `crates/uwumail-jmap/src/methods/sieve.rs` — JMAP.
- `crates/uwumail-imap/src/managesieve.rs` — ManageSieve.
