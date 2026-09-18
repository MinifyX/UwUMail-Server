# Security audit — the whole stack, 18 September 2026

The third pass, after [security-audit.md](security-audit.md) (before going public) and
[security-audit-2026-09.md](security-audit-2026-09.md) (server and gateway). Two things are new
here: everything built since the last one — reports, the spam history, the host helper, updates
over the tunnel and restoring a backup — and the **client**, which neither earlier audit covered.

Scope: `uwumail-smtp`, `uwumail-imap`, `uwumail-jmap`, `uwumail-store`, `uwumail-web` and `web/`,
`uwumail-backup`, `uwumail-server`, `uwumail-tunnel`, `uwumail-gateway`, `deploy/host`,
`deploy/gateway`, and in the `UwUMail` repository the Tauri desktop client with its mail rendering,
IPC and credential storage.

Done with Claude, not an independent firm. An honest sweep, not a certificate. No exploit code and
no attack payloads are written down here — the analysis says what is wrong and the diffs say how it
was fixed. Nothing was run against the production server; the test VM was used, and it may be taken
apart.

## Summary

| Severity | Found | Fixed | Open |
| --- | --- | --- | --- |
| Critical | 1 | 1 | 0 |
| High | 1 | 1 | 0 |
| Medium | 1 | 1 | 0 |
| Low | 6 | 6 | 0 |

The critical and the high one are both in IMAP, both reachable **before a login**, and both are
denial of service rather than a way in. The client came out clean: the places a mail client usually
goes wrong — rendering HTML, storing passwords, what the webview may reach — are built the careful
way, and the findings below are all on the server side.

**Why a crash counts as critical here.** `Cargo.toml` sets `panic = "abort"` for release builds
(line 108). There is no unwinding: a panic in the task handling one connection takes the entire
process with it — SMTP, IMAP, JMAP, the portal, the queue. A stack overflow does that even without
`panic = "abort"`, and cannot be caught at all. So for this server, "a stranger can make a parser
panic" and "a stranger can stop the mail" are the same sentence.

## Findings

### F-1 · Critical · A nested search takes the whole server down, without logging in

`crates/uwumail-imap/src/parser.rs:862` (`search_key`)

`(`, `NOT` and `OR` each make the SEARCH parser call itself, and nothing counted the levels. A
command line may be 64 KiB (`MAX_LINE`), which is tens of thousands of levels — far past any stack.
The parser runs in `Session::run` **between** reading the line and dispatching it, and dispatch is
where authentication is checked, so this is reachable by anyone who can open port 993.

A stack overflow is not a Rust panic. It cannot be caught, and it ends the process. Docker starts
the container again, and the next line stops it again.

Measured on the test build: around 170 levels was enough, in a line of a few hundred bytes.

**Fix:** count the depth in the parser and refuse past a limit no real client comes near.

```diff
+const MAX_SEARCH_DEPTH: usize = 32;
+
 struct Parser<'a> {
     input: &'a [u8],
     pos: usize,
     utf8: bool,
+    depth: usize,
 }

 fn search_key(&mut self) -> Parsed<SearchKey> {
+    self.depth += 1;
+    if self.depth > MAX_SEARCH_DEPTH {
+        self.depth -= 1;
+        return Err("the search is nested too deeply".into());
+    }
+    let key = self.nested_search_key();
+    self.depth -= 1;
+    key
+}
+
+fn nested_search_key(&mut self) -> Parsed<SearchKey> {
     if self.eat(b'(') {
```

The guard sits in front of the old body, so every way in counts: `(`, `NOT` and `OR` all come
through `search_key`.

**Tests:** `parser::depth_tests::a_search_nested_too_deeply_is_refused_rather_than_fatal` and
`not_and_or_are_counted_too`. Both call the parser at depths that used to end the process;
reaching the next line is the assertion.

### F-2 · High · A stranger can make the server set aside a message worth of memory per connection

`crates/uwumail-imap/src/session.rs:236` (`read_command`)

`APPEND` may carry a whole message, so a literal it announces is allowed up to `max_append`
(`smtp.max_message_size`, 50 MiB by default). The reader checked the announced size against that
limit and then reserved it — `command.resize(start + size, 0)` — **before** the bytes arrive and
before anyone has said who they are. The memory is held until the login times out (60 s), and the
sender only has to write one short line and then stay quiet. IMAP allows 2000 connections
(`MAX_CONNECTIONS`).

**Fix:** the generous limit belongs to people who are logged in. Nothing before a login needs more
than a command.

```diff
-let limit = if is_append { self.imap.max_append } else { MAX_COMMAND };
+let limit = if is_append && self.account.is_some() { self.imap.max_append } else { MAX_COMMAND };
```

**Test:** `before_a_login_a_literal_may_not_be_bigger_than_a_command` in
`crates/uwumail-imap/tests/imap.rs`. It connects without logging in, announces a literal under the
append limit and over `MAX_COMMAND`, and requires a refusal; then it logs in and requires that the
same size is welcomed. Without the fix it fails with *"a stranger was allowed it: + Ready for
literal data"*.

### F-3 · Medium · A report costs far more to read than it costs to send

`crates/uwumail-smtp/src/reports.rs:24`

DMARC and TLS reports arrive from anyone, as compressed attachments. Unpacking is capped at 8 MiB
(`MAX_UNPACKED_BYTES`, honoured by mail-auth) and four are read at once (`AT_ONCE`) — but the
message itself was only bounded by `max_message_size`, 50 MiB. The parser tries **every part** of
the message in turn and stops at the first one that reads as a report, so a message made of many
small, densely packed parts is unpacked many times over before it is given up on. The work is done
in `spawn_blocking`, so the runtime keeps going, but four cores can be kept busy for a long time by
one message.

**Fix:** a message this size is not a report. Refuse it before the semaphore, which also removes
the `raw.clone()` for it.

```diff
+const MAX_REPORT_BYTES: usize = 4 * 1024 * 1024;
+
 pub(crate) fn receive_soon(...) {
+    if raw.len() > MAX_REPORT_BYTES {
+        tracing::info!(%address, size = raw.len(), "ignored a report far too big to be one");
+        return;
+    }
     let Ok(permit) = ctx.reports.clone().try_acquire_owned() else {
```

A real aggregate report is a few kilobytes; the largest senders stay well under a megabyte.

**Suggested test:** hand `receive_soon` a message over the cap and assert that nothing reaches the
store. Not written yet — it needs a `Context`, which the current tests build only end to end.

### F-4 · Low · An announced literal length can wrap the position

`crates/uwumail-imap/src/parser.rs:226` (`literal`)

`number()` accepts up to twenty digits, and `self.pos + size` was unchecked. In the release profile
integer overflow wraps and the following `get(pos..wrapped)` returns `None`, so the shipped binary
answers with an error — safe by accident. In a debug or test build it panics. With `panic = "abort"`
and `overflow-checks` one setting away, "safe by accident" is not where this should sit.

**Fix:**

```diff
-let data = self.input.get(self.pos..self.pos + size).ok_or("the literal is shorter than announced")?;
+let end = self.pos.checked_add(size).ok_or("the literal is longer than this server can hold")?;
+let data = self.input.get(self.pos..end).ok_or("the literal is shorter than announced")?;
```

**Test:** `parser::depth_tests::an_absurd_literal_length_does_not_wrap_the_position`.

Found by the randomized harness (below), not by reading.

### F-5 · Low · The version handed to the root helper was never checked on this side

`crates/uwumail-server/src/host.rs:79`

`HostBridge::ask` wrote the verb and version straight into `jobs.jsonl`. The version comes from
GitHub's release list by way of `newer_releases`, which takes `tag_name` and trims a leading `v` —
whatever the API says. The helper checks the shape itself and would have refused anything else, so
nothing was exploitable; but the helper runs as root and this is the layer that hands it work, and
`routes/updates.rs` claims a version the server never heard of does not get that far. The gateway's
side (`Machine::ask`) already checked both. The two sides should be alike.

**Fix:** check the verb against the same fixed list and the version against the same shape before
writing the job, mirroring `crates/uwumail-gateway/src/machine.rs`.

### F-6 · Low · The helpers would install an older version

`deploy/host/helper:208`, `deploy/gateway/hardening/helper:236`

Both helpers accepted any well-formed version, including one older than what runs. The portal only
ever offers something newer — but the portal runs in the container, which is the side the helper
exists to distrust. A server somebody took over could ask to be rolled back to an old image and walk
back in through a hole that was already closed.

**Fix:** an `is_older` check in both, refusing to go backwards. When either side is not a version (a
tag like `latest`), there is nothing to compare and the update goes ahead.

```bash
is_older() {
  local want="${1:-}" have="${2:-}"
  is_version "$want" && is_version "$have" || return 1
  [ "$want" = "$have" ] && return 1
  [ "$(printf '%s\n%s\n' "$want" "$have" | sort -V | head -1)" = "$want" ]
}
```

Checked by hand against `0.1.0 → 0.2.2` (refused), `0.10.0 → 0.9.0` (allowed — a string compare
would get that one wrong) and `0.2.2 → latest` (allowed).

### F-7 · Low · Two clicks on restore could empty the staging directory the first one is filling

`crates/uwumail-backup/src/service.rs:242` (`start_restore`)

The check that no restore is running and the setting of that state were separated by an `await`, so
two requests a moment apart could both get through. The second one then removes the staging
directory the first is still writing into. It fails safe — the take-over at the next start notices
the missing database and touches nothing — but it is a race, and it needs an admin, so the damage is
self-inflicted rather than an attack.

**Fix:** claim the slot inside the mutex, before anything is awaited, and give it back if the
settings turn out to be missing.

### F-8 · Low · The audit log recorded a restore that was refused

`crates/uwumail-web/src/routes/backups.rs:284`

`audit(...)` was written before `start_restore`, so a refused restore left an entry saying it had
happened. The audit log is the one record of what was done to this server; it should not say things
that are not so. `routes/host.rs` and `routes/updates.rs` already write theirs afterwards.

**Fix:** move the entry after the call that can fail.

### F-9 · Low · A download could lose the header that keeps it from being rendered

`crates/uwumail-jmap/src/blob.rs:113`

The JMAP download endpoint lets the caller choose the response's content type (`?accept=`, only
checked for a `/`). That is safe because the answer is also `Content-Disposition: attachment` and
`X-Content-Type-Options: nosniff`, so the browser saves it rather than rendering it on the portal's
own origin. But the disposition header was set inside `if let Ok(value) = HeaderValue::from_str(...)`
— if the file name had ever failed to build a header, the protection would have fallen away in
silence. It cannot today (`encode_filename` percent-encodes everything outside a safe set), so this
is fragility rather than a hole.

**Fix:** always set the header, falling back to a bare `attachment` when the name will not fit.

## Fuzzing

`cargo fuzz` needs nightly and libFuzzer and does not support Windows, and a test that only runs
somewhere else is a test that stops running. So the harness is in the repository instead, on stable,
in CI: `crates/uwumail-imap/tests/robustness.rs`.

It takes a corpus of real IMAP commands and mutates them the ways that break parsers — bit flips,
truncation, long runs, tampering with an announced literal length, removing or doubling line
endings, splicing two commands together, bytes no text protocol expects, and deep nesting — plus
pure noise every fourth round. A fixed seed makes a failure reproducible from the output;
`UWUMAIL_FUZZ_ROUNDS` makes a local run longer. Every parser a stranger reaches is called on each
input, and a round taking more than a second fails the test, because quadratic is as good as a crash
when anyone can send it.

It found **F-1 and F-4 in its first run.** After the fixes: 400 000 rounds in release and 150 000 in
debug (where arithmetic overflow still panics) came back clean.

Worth adding later: the same harness over the SMTP command reader and the MIME parser in
`uwumail-smtp`, and over the JMAP request shape. `mime::parse` in `uwumail-imap` already limits its
depth and part count and was not the problem.

## What held up

- **No `unsafe` anywhere** in the server workspace. The only `unsafe` string in it is
  `style-src 'unsafe-inline'` in a CSP.
- **`cargo audit`** runs in CI on every commit and is green: no known advisories in the tree.
- **Sessions:** `__Host-` prefixed cookie over HTTPS, `HttpOnly`, `SameSite=Strict`, CSRF token
  required on everything that is not GET or HEAD, compared in constant time.
- **Passwords:** argon2, with a dummy hash verified when the account does not exist, so the answer
  takes the same time either way and cannot be used to find out who has an account.
- **SQL:** every value is bound as a parameter, including the search filters; sort expressions come
  from typed enums, never from strings. FTS5 terms are quoted so a search cannot use FTS syntax.
- **`X-Forwarded-For`** is read only from an address in `http.trusted_proxies` (empty by default),
  and the chain is walked from the right past our own proxies. An untrusted proxy is reported once,
  loudly, with the line to add.
- **MTA-STS** really does what the documentation says: an enforced policy sets `verified_tls`, which
  picks the TLS configuration that checks the certificate and makes TLS mandatory.
- **Tunnel:** both sides pin the other's certificate by fingerprint, handshake signatures are always
  verified, the pairing token is compared in constant time and its `Debug` is redacted, and refusals
  are rate-limited per address.
- **JMAP access control:** `accountId` is checked against the session's own on every method, and a
  blob download checks that the blob belongs to the account before it is read.
- **Secrets are not logged.** Nothing in the tree logs a password, a token or a key value.
- **The client:** mail renders in an iframe with `sandbox="allow-same-origin"` and **no
  `allow-scripts`**, so mail can never run code even if the sanitiser were bypassed; the frame
  carries its own `default-src 'none'` CSP; DOMPurify runs over the HTML, and a second, stricter
  pass strips styles and remote sources for quoted text in the composer. Remote images are off until
  they are allowed. Links are intercepted and go through the phishing check. `connect-src` is
  `ipc:` and `asset:` only, so the webview cannot reach the network by itself. Tauri capabilities
  are limited to the main window with `opener` scoped to `http`, `https` and `mailto`; the asset
  protocol is scoped to two directories. Passwords and refresh tokens live in the OS keychain, and
  `Secret`'s `Debug` is redacted with a test that says so. Credentials follow a redirect only within
  the same site, decided with the public suffix list, and never from HTTPS down to HTTP.

## Accepted, and worth knowing

- **`panic = "abort"`.** Deliberate, and it makes every reachable panic an outage. The randomized
  harness is the answer to that, and it should grow to cover the SMTP and JMAP entry points too.
- **A session younger than ten minutes skips the password** for sensitive actions
  (`confirm_identity`). That now covers updating the server, restarting the machine and restoring a
  backup, which are heavier than what it was written for. It is still the same rule as changing a
  password or pairing a gateway, and the alternative is asking for the password twice within a
  minute of logging in.
- **The shared directory with the host helper** is `root:10001`, mode `0770`. Anything else on that
  machine running as uid or gid 10001 could write jobs. That is the accepted price of a bridge made
  of files, and it is why the helper checks every field itself.
- **The failed-login line logs the login that was typed** (`routes/auth.rs`). Someone who types
  their password into the address field puts it in the log, which admins can read in the portal. It
  stays because knowing which address was tried is what makes the line useful.
- **The gateway's buttons need a release.** A gateway older than this does not announce that it can
  take jobs, and the gateway is only built for releases — so the first gateway update after this one
  is still done by hand.
- **Addons are not implemented yet.** `frame-src` already allows `uwuaddon:`, which is harmless
  while nothing serves it. When the host is built, the question to settle first is that an addon
  frame inside the main window must not reach Tauri's IPC: capabilities are per window, not per
  frame.

## To do, in order

1. **Done — ship it.** F-1 and F-2 are both remote, both without a login, and both stop the mail.
   The release that carries these fixes is worth making promptly.
2. **Grow the harness** to the SMTP command reader and the MIME parser in `uwumail-smtp`, and to the
   JMAP request shape. F-1 was in the one parser that had no fuzzing; the others have had no more.
3. **Consider `overflow-checks = true`** for the release profile, now that `checked_add` is in the
   one place that needed it. It turns silent wrapping into a loud failure — but with
   `panic = "abort"` it also turns an arithmetic slip into an outage, so it wants the harness to be
   broader first.
4. **A test for F-3**, once there is a cheap way to build a `Context` in a unit test.
5. **Re-check the accepted list** when addons land, and when the confirmation window next comes up.
