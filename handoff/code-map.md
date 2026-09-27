===== a78926d537f7d20ce
A server release is a PR from a working branch into `main`. Its last commit is `chore(release): X.Y.Z`. After the merge, the tag `vX.Y.Z` is pushed on the merge commit, and CI builds the images and creates the GitHub release. No script does this, and the webmail has no release process of its own.

## Workflows (`/home/user/UwUMail-Server/.github/workflows/`)

**`ci.yml`** runs on:
- pushes to `main`,
- pushed tags `v*`,
- `pull_request`,
- `workflow_dispatch`.

Pushes that only touch `docs/**`, `brand/**`, `**/*.md` or `.github/ISSUE_TEMPLATE/**` are skipped. Pull requests skip the first three of those; the issue-template path is not excluded for them. A newer run on the same ref cancels the older one.

| Job | Runs on | What it does |
|---|---|---|
| `test` | always | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo test --workspace --locked`; `node --check dev/smoke.mjs scripts/live-check.mjs`; `bash -n` and `shellcheck --severity=warning` on every tracked `*.sh` and the deploy helpers; `bash deploy/tests/helpers.sh` |
| `audit` | always | `cargo audit` |
| `web` (in `web/`) | always | `pnpm install --frozen-lockfile`, `format:check`, `typecheck`, `lint`, `test` (vitest), `build`; fails if `dist` contains `uwu.example`; uploads `web-dist` |
| `webmail` | not on PRs | Clones the repo and commit from `webmail.pin` (`--depth 1`, checks the hash, no credentials), runs `pnpm install --frozen-lockfile && pnpm build`, uploads `webmail-dist` |
| `gateway` | not on PRs; needs test, audit | Builds the amd64 gateway with `docker/Dockerfile.gateway`, checks `--version`, uploads it with a sha256 |
| `image` | not on PRs; needs test, audit, web, webmail | See below |
| `release` | tags `v*` only; needs image, gateway | See below |

**The `image` job** does this in order:
1. Downloads both dist folders.
2. Cross-builds `cargo build --release -p uwumail-server` for x86_64 and aarch64.
3. Greps the binaries to confirm the portal and webmail assets are embedded.
4. Builds an amd64 image and scans it with Trivy; any fixable HIGH or CRITICAL finding fails the job.
5. Pushes a multi-arch image to **`ghcr.io/minifyx/uwumail-server`** using `docker/Dockerfile.release` (distroless base).

Image tags:
- `edge` for commits on the default branch.
- `beta` for every `v*` tag.
- `sha-<7 characters>` always.
- The semver tags `{{version}}` and `{{major}}.{{minor}}`. The workflow comment says stable releases also get `latest`, and metadata-action's default adds that for non-prerelease semver tags.

`UWUMAIL_RELEASE` is set to the tag name so the server checks for newer releases rather than newer commits.

**The `release` job** does this in order:
1. Fails unless the first `version =` in `Cargo.toml` equals the tag without its `v`.
2. Packages the gateway tarball (binary, `install.sh`, `gateway.toml`, the systemd unit, `hardening/`).
3. Packages `install.sh`, `update.sh`, `compose.yaml` and `.env.example` (as `env.example`), plus the host helper tarball `deploy/host`. Every file gets a `.sha256`.
4. Uses awk to extract the `## <version>` section of `CHANGELOG.md` into the notes, and fails if it is empty.
5. Runs `gh release create vX.Y.Z --title "UwUMail Server X.Y.Z" --notes-file notes.md …`. A tag containing `-` gets `--prerelease`.

**Other server workflows:**
- `image-scan.yml`: a weekly cron (Monday 06:17) plus manual start; Trivy-scans `:edge`.
- `secret-scan.yml`: gitleaks on pushes to `main`, on PRs and on manual start.

## A release, step by step (from the git history and PRs)

1. Work happens on a branch: `feat/0.12` (with feature branches merged into it) or `claude/<name>`. PR #17 came from `claude/dazzling-einstein-2g1eol`.
2. The last commit on the branch is `chore(release): X.Y.Z`. It bumps `Cargo.toml` `[workspace.package] version` and `Cargo.lock` (10 crate entries), and turns `## Unreleased` into `## X.Y.Z` in `CHANGELOG.md`, or adds that section directly. When needed it also changes `webmail.pin` and `docs/roadmap.md`. The commit body sums up the release.
3. A PR into `main` titled `X.Y.Z` or `X.Y.Z: <summary>` is merged with a merge commit: `Merge pull request #N: X.Y.Z`, with the summary as the body, e.g. `0.13.0: calendars and contacts from elsewhere`. CI on `main` then publishes `:edge`.
4. `vX.Y.Z` is tagged **on the merge commit** and pushed:
   - `v0.13.0` is annotated and points to `186cb72`.
   - `v0.12.0` is annotated and points to `22b932e`.
   - `v0.12.1` and `v0.12.2` are lightweight tags on `71643e5` and `b70695f`.
5. The tag push runs CI again. This build produces the version image tags and the GitHub release.

`docs/deployment.md` (around lines 448–452) is the only written description: "Releases come from tags: I add a section for the version to `CHANGELOG.md`, push the tag `v0.1.0` (or `v0.2.0-beta.1`), and CI builds the image … and publishes a GitHub release…". `CONTRIBUTING.md` and `docs/development.md` say nothing about releases. They only list fmt/clippy/test, the pnpm checks, conventional commits, and that `:edge` follows `main`.

## Files changed in release commits

| Commit | Files |
|---|---|
| `f9e397d` 0.13.0 | `CHANGELOG.md` (+7/−1), `Cargo.lock`, `Cargo.toml` (`0.12.2` → `0.13.0`) |
| `6420442` 0.12.2 | `CHANGELOG.md` (new section), `Cargo.lock`, `Cargo.toml`, `webmail.pin` (`f7af3bd…` → `13dbd1181e6c65ce44dd27f719bb7fd2369b2d79`) |
| `c3d1567` 0.12.0 | `CHANGELOG.md` (+83), `Cargo.lock`, `Cargo.toml`, `docs/roadmap.md` (items checked off), `webmail.pin` |

`web/package.json` is **not** bumped in releases. It stays at `"version": "0.2.2"` and was last touched by the 0.6.0 release commit.

## Webmail pin and bundling

`/home/user/UwUMail-Server/webmail.pin` contains `repo=https://github.com/MinifyX/UwUMail-Webmail.git` and `commit=<full hash>`. Its header comment says: "To move to a newer webmail: put its commit here, let CI build once, and say so in the changelog." `docs/webmail.md` (around lines 75–81) says the same.

`crates/uwumail-web/build.rs` embeds `UWUMAIL_WEBMAIL_DIST`, which defaults to `../../webmail/dist`. If that folder is missing, the build simply has no `/mail`. CI's grep in the `image` job guards against that.

## CHANGELOG style

The header reads: "Each release gets a section here before its tag is pushed; CI copies the section into the GitHub release. Versions follow semver; `-beta.N` versions are pre-releases."

Features collect under `## Unreleased` and are renamed in the release commit. Each section is prose with bold lead-ins, bullet lists, doc links, a migration note, and a closing `**Updating.**` paragraph that includes the webmail status. The 0.13.0 entry, verbatim:

```
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
```

## Tags

The local clone is shallow and has no tags, so `git tag` prints nothing. The last tags on origin (from `git ls-remote --tags`): `v0.7.1`, `v0.8.0`, `v0.9.0`–`v0.9.3`, `v0.10.0`, `v0.11.0`, `v0.12.0`, `v0.12.1`, `v0.12.2`, `v0.13.0`.

## Scripts

There are no release scripts.
- `scripts/deploy.sh` pulls a registry tag (default `edge`) on a host over SSH.
- `scripts/deploy-local.sh` builds locally and copies the image over SSH.
- `scripts/live-check.mjs` and `dev/{smoke.mjs,seed.sh,compose.yaml,client-compat.sh}` are for manual testing.
- `install.sh` and `update.sh` are the files users download from the release.

## Test weight

- About 688 `#[test]`/`#[tokio::test]` functions. Each crate's integration tests build into one binary (`crates/*/tests/integration/main.rs`, since commit `0c3f15c`).
- Nothing in the Rust test code calls Docker. The SMTP flow test starts two servers inside the test process, and ClamAV is replaced by a stand-in.
- The weight is compile time and disk: `0c3f15c` says `target/` used to reach about 50 GB and is now under 6 GB.
- The only Docker in CI is the gateway build and the image build/scan, which don't run on PRs.
- The release build is heavy: two `--release` cross-compiles plus a multi-arch image build.

## The other two repos

**`/home/user/UwUMail-Webmail`**
- No tags and no release workflow. `package.json` stays at `0.1.0`.
- `ci.yml` runs on pushes to `main`, on PRs (both skip `**/*.md`) and on manual start: `pnpm install --frozen-lockfile`, `format:check`, `typecheck`, `lint`, `test`, `build`, `pnpm audit --prod`.
- It is "released" only by the server moving its pin to a commit on `main`, such as the merge commit `13dbd11` (PR #8).

**`/home/user/UwUMail-Client`** has its own separate flow. `release-notes/README.md` gives the steps:
1. Bump `Cargo.toml`, `Cargo.lock`, both `tauri.conf.json` files and the `package.json` files, and add `release-notes/<ver>.json` (German and English). Commit `8d5f808` (`chore: release 0.5.0-beta.3`) is an example.
2. Merge the PR and wait for the Android and iOS workflows on `main`.
3. Push `v<ver>`. `release.yml` (tags `v*`) builds, signs and publishes every platform, and updates the feeds on the `updates` branch and the AUR package.

`scripts/release.mjs` is a manual fallback for Windows. The latest tag is `v0.5.0-beta.3`.
===== a2bf1442f4980f636
I mapped all four areas. Three points you should know first: `egress.rs` is not the SMTP TLS path, `rules.rs` is not about addresses, and the `addresses.kind` column has a CHECK constraint that limits what new address kinds can be added cheaply.

## 1. Directory: how addresses are modelled

**Where things are.** There is no `rules.rs` for addresses. `uwumail-store/src/rules.rs` holds spam sender/word-list rules. `uwumail-smtp/src/rules.rs` runs Sieve at delivery. The address model is spread over `directory.rs`, `forward_addresses.rs`, `own.rs`, `extras.rs`, `admin.rs`, `acl.rs` and `sharing.rs` in `/home/user/UwUMail-Server/crates/uwumail-store/src/`.

**Tables** (all in `crates/uwumail-store/src/migrations/`):
- **Accounts and aliases** (`0001_initial.sql:30,44`): `domains`, `accounts` and `addresses`.
  - `addresses` has `local_part`, `domain_id`, `account_id`, `created_at` and `UNIQUE(local_part, domain_id)`.
  - Its `kind` column is limited to `'primary'` or `'alias'` by a CHECK.
  - SQLite cannot widen a CHECK without rebuilding the table. Migration 0025 avoided that for accounts by adding a separate column (`accounts.kind`) instead. A "masked" address kind would need the same trick: a new column or a new table.
- **Self-service aliases** (`0007_self_service.sql`): `addresses.created_by_owner`, `domains.self_service_aliases`, `accounts.alias_limit`, `released_addresses` (a deleted alias stays reserved for 30 days), and `forward_targets` (a person's own forwarding, confirmed by link).
- **Catch-all:** the column `domains.catch_all_account_id`.
- **Forwarding addresses** (`0019_forward_addresses.sql`): `forward_addresses(local_part, domain_id, targets TEXT, note, created_at)`. `targets` holds one address per line and has no account.
- **Send-as** (`0020_send_as_domains.sql`): `send_as_domains(account_id, domain_id)`.
- **Service accounts** (`0025_service_accounts.sql`): `accounts.kind` ('person' or 'service'), per-protocol `*_enabled` columns, and `redirect_to`.
- **Folder sharing** (`0037_mailbox_acl.sql`): `mailbox_acl(mailbox_id, owner_id, grantee_id, rights, …)`, with `PRIMARY KEY(mailbox_id, grantee_id)` and `CHECK(owner_id <> grantee_id)`.
- **Calendar/contacts sharing:** `dav_shares` (`0038_…`).

**Key functions:**
- `directory.rs:298` `resolve(conn, address) -> Option<i64>` checks, in order:
  1. The exact address in `addresses`.
  2. The base address with the `+tag` removed (`address.rs:54` `base_local_part`).
  3. A forwarding address. If one exists it returns `None`, so the catch-all cannot take its mail.
  4. The domain's catch-all.
  5. For `postmaster@`/`abuse@`, the first admin.
- `directory.rs:669` `delivery_target(account_id)`: a service without a mailbox hands its mail on via `redirect_to`, one hop only.
- `directory.rs:764/803` `add_alias`/`remove_alias` (admin), and `own.rs:129/183` `create_own_alias`/`delete_own_alias` (the person, with limit, reserved names and release period).
- `forward_addresses.rs:28` `address_in_use(conn, local, domain_id)` is the collision check across both address tables. Any new address table must be added to it and to the collision queries in `own.rs:147-152`, `directory.rs:586` and `delete_domain` at `directory.rs:392`.
- `forward_addresses.rs:39` `forward_targets` (also honours `+tag`) and `:162` `forward_address_targets` → `Vec<(target, Option<local account id>)>`.
- `extras.rs:83` `owns(conn, account_id, email)` is the single send-as check. It allows the account's own addresses plus `+tag` variants, any address of a `send_as_domains` domain, and fetched accounts that have sending set up.
- `extras.rs:151` `identities()` creates default JMAP identities from `addresses` the first time they are read.
- `acl.rs`: `ALL_RIGHTS = "lrswipkxtea"`, `ShareLevel`, `set_mailbox_acl_for` (bumps the owner's modseq and records a `Mailbox` change), `mailboxes_shared_with`, `share_people`. `ACTIVE_PERSON` (`kind <> 'service' AND deleted_at IS NULL`) keeps services out of sharing.

**Inbound SMTP** (`crates/uwumail-smtp/src/inbound.rs`):
- `Recipient` struct at `:393` has fields `local_account`, `srs_return`, `report`, `forward_to: Option<Vec<(String, Option<i64>)>>` and `trap`.
- `rcpt()` at `:1011` checks in order: SRS bounce address (`:1028`), report address via `store.report_recipient` (`:1066`), spam trap (`:1086`), `forward_address_targets` (`:1103`), `resolve_recipient` (`:1119`). A service without a mailbox goes through `delivery_target` (`:1145`), and quota is checked at `:1152`.
- `receive()`:
  - `:1567` a forwarding address calls `forward::send(ctx, Forwarder{name, account_id: None}, …)`; spam is not passed on.
  - `:1578-1672` a person's delivery goes through greylist hold, sender lists and per-person Bayes, then `forward::plan`, then Sieve (`rules::deliver`) or `store.ingest(IngestRequest{…Inbox/Junk})`.
- `forward.rs:43` `send()` delivers local targets through `delivery_target` + `ingest` straight to the Inbox. It skips their own forwarding and Sieve. Remote targets are queued with SRS. Loops are caught with a `Delivered-To` header.
- The submission path repeats the same logic in `submission.rs:224-301`, and the send-as check is at `submission.rs:161-171`.
- Other callers of `resolve_recipient`: `dsn.rs:34`, `vacation.rs:111`, `scheduling.rs:137`, `rules.rs:243`, `dav/src/lib.rs:859`, `web/routes/mailbox.rs:68`, `jmap/methods/calendar.rs:405`, `server/import/mailcow.rs:422`.

**Admin and portal routes** (`crates/uwumail-web/src/lib.rs`):
- Catch-all: `:402`. Forwarding addresses: `:403`. Send-as domains: `:473`. Aliases: `:476`.
- Self-service aliases: `routes/own.rs`. Sharing: `routes/sharing.rs`.

**What already exists for ACL and sharing:**
- **IMAP:** ACL, `RIGHTS=kxte` and NAMESPACE `Shared/<login>/…` in `uwumail-imap/src/mailboxes.rs:10-140`, `session.rs:380,995,1027` and `parser.rs:558`.
- **JMAP:** `uwumail-jmap/src/sharing.rs`. A shared owner appears as its own account with `isPersonal:false` (`add_to_session` `:281`). `SHARED_METHODS` (`:23`) is the allowlist of methods usable there; `enter`/`leave` (`:90/:132`) swap `ctx.account`; `SharedView` holds per-mailbox rights. Mailbox `shareWith` follows RFC 9670, and Principal get/query/changes is in `methods/principal.rs` (`type` is always `"individual"`).
- **`docs/sharing.md`:**
  - A share is per-folder, person to person.
  - Storage and quota stay the owner's, and flags (including `\Seen`) are shared by everyone.
  - Submitting from a shared account gives `accountNotSupportedByMethod`, and `maySubmit=false`.
  - `docs/roadmap.md:119` lists "Groups, shared mailboxes, masked addresses" as open.

**Where each planned feature would hook in:**
- **Groups** (one address, several members):
  - Forwarding addresses already do this for plain fan-out: `targets` can be local accounts, `forward::send` ingests into each member's Inbox, and there is a test in `flow.rs:495`.
  - A real group needs a members table keyed by account (`group_members(group_id, account_id)`). It would plug in at `directory::resolve` (or a sibling lookup like `forward_address_targets`), at `rcpt()` near `inbound.rs:1103`, at the per-recipient loop in `receive()`, and at `submission.rs:283`.
  - Per member you'd likely want their own spam decision and Sieve, which `forward::send` skips today, plus a `Forwarder` without an account.
  - Also needed: group send-as in `extras::owns`, collision checks in `address_in_use`, optionally a principal of type "group" in `methods/principal.rs`, and admin routes.
- **Shared mailboxes:**
  - The closest existing piece is a service account: `kind='service'`, a mailbox, no portal login.
  - But `ACTIVE_PERSON` in `acl.rs:137` deliberately keeps services out of sharing, so either change that filter or add a new `accounts.kind` value (new column or CHECK workaround again).
  - Access then comes from `mailbox_acl` on all its folders, through the IMAP `Shared/` namespace and the JMAP shared account.
  - Sending "as" it goes through `extras::owns` (a per-address grant; today send-as only works per domain). `maySubmit` is hard-coded false (`sharing.rs:143` `rights_json`). JMAP Identity for the shared address would be needed too.
- **Masked addresses:**
  - Needs a new table, for example `masked_addresses(id, account_id, local_part, domain_id, state, for_domain, description, url, email_prefix, created_by, created_at, last_message_at)`. `addresses.kind` cannot take a new value without a rebuild.
  - Hooks:
    - Resolve: `directory::resolve` and `forward_addresses::address_in_use`.
    - Disabled/deleted state: the fastmail semantics would be answered at `rcpt()`.
    - Send-as: `extras::owns`, plus identities in `extras.rs:151`.
    - Reservation: reuse the `released_addresses` rule in `own.rs`.
    - Reserved names: `own.rs` `RESERVED`.
    - Report addresses: `report_recipient` only stays out of the way when an `addresses` row exists.
  - The JMAP side is covered in section 4.

## 2. Outbound TLS

**Wrong file:** `egress.rs` is only the HTTP(S) proxy layer for remote pictures, logos, update checks and fetch. SMTP delivery is `outbound.rs` together with `client.rs`, `tls.rs` and `mta_sts.rs`.

**Delivery path:**
- `outbound.rs:25` `run_queue` → `deliver_group` (`:91`) → `deliver_domain` (`:308`) → `resolve_targets` (`:220`) → `session` (`:341`).
- `Target{host, addrs, via, verified_tls}` (`:164`). `verified_tls` is true only when the domain has an **enforced** MTA-STS policy (`:269`).
- In `session`, STARTTLS picks `ctx.client_tls.verified` when the target is a relay (not set to "none") or has `verified_tls`; otherwise it uses `opportunistic` (`:393-414`). A TLS failure becomes `Err(format!("TLS: {e}"))`, and the next host is tried.
- **MTA-STS in `testing` mode is fetched and then ignored:** only `Mode::Enforce` is acted on, and nothing is recorded.

**Recording the TLS result of each delivery: nothing is recorded.**
- `Outcome` is only `Delivered|Deferred|Failed(String)` (`:67`).
- The store keeps only `queue_recipients.status/last_error` (`0001_initial.sql:168`), via `mark_recipient_delivered/deferred/failed` in `queue.rs:213-222`.
- `health::DeliveryStats` (`health.rs:~95-160`) is in memory only: events plus `trouble(domain, route, ProbeStage, error)`, with no TLS fields.
- There are no TLS-RPT tables or code for sending reports. Roadmap line 126 has "Sending TLS reports to other domains, DANE" open.

To send TLS-RPT reports you would need to:
- Return a per-session TLS outcome from `session()`: policy type (`sts`, `tlsa` or `no-policy-found`), policy string/MX list, result type (e.g. `starttls-not-supported`, `certificate-expired`, `validation-failure`, `sts-policy-fetch-error`, `tlsa-invalid`, `dnssec-invalid`), receiving MX host and IP, and the sending IP.
- Also capture MTA-STS fetch failures in `mta_sts::policy_for` (`mta_sts.rs:158-192`), which today only logs them.
- Store daily counters in a new table (e.g. `tls_rpt_sessions(domain, day, policy…, result_type, mx_host, receiving_ip, count)`).
- Add a daily job that looks up `_smtp._tls.<domain>` TXT via `ctx.authenticator.txt_lookup` and sends by `mailto:` (queue) or `https:`.
  - `mail-auth` already includes `report::tlsrpt::{TlsReport, PolicyType}` (used for parsing in `smtp/src/reports.rs:7,126`), which could likely also build the report.
  - Hourly housekeeping runs in `uwumail-server/src/serve.rs:321` `collect_garbage`, where `purge_reports` is called at `:347`. Tasks are spawned at `serve.rs:111`.

**Reading incoming reports:**
- `smtp/src/reports.rs` has `receive_soon` (`:37`) and `tls_report` (`:126`).
- `store/src/reports.rs` has `report_recipient` (`:267`, local parts `tls-reports`/`dmarc-reports`), `add_tls_report` (`:387`), `cached_sts_policy`/`cache_sts_policy` (`:344/368`), and `purge_reports`.
- Tables: `tls_reports`, `tls_report_failures`, `mta_sts_policies`, `dmarc_*` (`0008_mta_sts_reports.sql`, extended in `0023_report_detail.sql`).

**DNS:**
- Main resolver: `mail_auth::MessageAuthenticator` (`mail-auth = "0.13"`), built in `smtp/src/lib.rs:184` with `new_system_conf()`, falling back to `new_quad9_tls()`. MX, IP and TXT lookups are cached through `dns.rs` `DnsCaches`.
- Directly: `hickory-resolver = "0.26"` with `default-features=false, features=["tokio","recursor"]` (`Cargo.toml:49`). It is only used in `dnscheck.rs` (a `Recursor` from the root servers plus `TokioResolver`) and `pictures.rs`.
- **Is DNSSEC validation available for DANE? Not confirmed.**
  - No `dnssec-*` feature is set in the workspace, and there is no TLSA or DNSSEC code anywhere.
  - mail-auth's `RecordSet` has a `dnssec_status: DnssecStatus` field, which the code only ever fills with `Indeterminate` in test pins.
  - `Cargo.lock` shows `hickory-proto 0.26.3` depending on both `ring` and `aws-lc-rs`, which hints that some DNSSEC or TLS feature is switched on somewhere in the dependency tree, probably via mail-auth.
  - I couldn't check which features are actually active: `cargo tree --offline` failed because crate sources aren't available. Run `cargo tree -e features -i hickory-proto` with network access.
  - For DANE you would enable hickory's `dnssec-aws-lc-rs` feature, validate TLSA answers (AD bit / validating resolver), and check certificates against TLSA in a custom verifier.

**TLS connections:**
- rustls 0.23 with aws-lc-rs and tokio-rustls 0.26, in `client.rs:99` `tls_handshake(config, host)`.
- `tls.rs`: `ClientTls{opportunistic, verified}`. `opportunistic` uses the custom `AcceptAnyCertificate` verifier (`tls.rs:41`), which checks signatures but accepts any certificate. `verified` uses webpki-roots.
- **A DANE verifier would be a third `ServerCertVerifier` next to these two**, fed the TLSA RRset per target.

## 3. Migrations and test setup

**Migrations:**
- 39 files in `crates/uwumail-store/src/migrations/`, named `NNNN_snake_name.sql` (`0001_initial.sql` … `0039_calendar_subscriptions.sql`).
- They are listed by hand in `db.rs:9-49` `MIGRATIONS` via `include_str!`, so a new file must be added there.
- `migrate()` (`db.rs:102`) applies each pending one in its own transaction and bumps `PRAGMA user_version`. It refuses to open a database newer than the build.
- Each file starts with an explanatory SQL comment. Style: `ALTER TABLE … ADD COLUMN` with defaults, and a new column instead of rebuilding a table (see the note in 0025).
- `db.rs:161-237` has tests that apply every migration to a fresh database and upgrade a 0.11 database (`RELEASED_0_11 = 35`). Update the table-exists assertion when you add tables.

**Store tests:**
- `lib.rs:292` `test_support::store()` gives `(Store, TempDir)` using a real `Store::open(tempdir)`. Used as `use crate::test_support::store;`.
- Store methods follow `self.read(|conn| …)` / `self.write(|tx| …)` on the blocking pool.
- JMAP-visible writes call `next_modseq`, `record_change(tx, account, modseq, kind, id, "created|updated|destroyed")`, then `notify_change`.
- Errors: `StoreError::{NotFound, Conflict, Invalid, Rule{code,…}}`.

**SMTP integration tests** (`crates/uwumail-smtp/tests/integration/`, one binary via `main.rs` with modules `flow`, `gateway`, `imip`):
- `flow.rs:48/57` `start(domain, users, routes)` / `start_with_spam` builds a tempdir store; the first user is Admin.
- `SmtpSettings` uses `DeliveryConfig.routes` mapping domain → `SocketAddr`, so two in-process servers deliver to each other. Self-signed TLS comes from rcgen (`server_tls`).
- `smtp.dns_cache().pin_no_txt(...)` keeps DNS lookups local. There are also `pin_txt`, `pin_mx`, `pin_ipv4/6` and `pin_ptr` in `dns.rs:106-160`.
- MX, Submission and SubmissionTls listeners are spawned along with `run_queue` and `run_learning`.
- Helpers: `inbox`, `mailbox`, `wait_for_inbox`, `raw`, `mailer` (lettre), `RawSession` (`:290`) for raw SMTP commands.
- Example tests: `forwarding_addresses_pass_mail_on_without_a_mailbox` (`:495`), service without mailbox (`:531`), reports (`:606`).

**JMAP integration tests** (`crates/uwumail-jmap/tests/integration/`, one binary via `main.rs`):
- Files: `api`, `calendar_sharing`, `calendars`, `common`, `conformance`, `contacts`, `query_changes`, `sharing`, `sieve`, `signatures`, `submission`, `suggestions`, `tokens`, `websocket`.
- `common/mod.rs` `server()` gives a tempdir store with domain `example.org` and users `mini`/`nyu` (password `katzenpfote-123`), then `Jmap::new(smtp).router()`.
- Requests are driven with `tower::ServiceExt::oneshot` and Basic auth. Helpers: `api(login, calls)`, `account_id`. `USING` lists the capabilities per file.

## 4. Adding a JMAP capability (e.g. MaskedEmail)

Steps, all under `/home/user/UwUMail-Server/crates/uwumail-jmap/src/`:
1. **Constant:** add `pub const MASKED_EMAIL: &str = "https://www.fastmail.com/dev/maskedemail";` (and/or `urn:ietf:params:jmap:maskedemail`) next to the others in `session.rs:16-40`.
2. **Session document:** `session.rs:83` `document()` fills `capabilities`, `accounts[id].accountCapabilities` and `primaryAccounts`. Conditional capabilities follow the calendars/contacts pattern at `:173-193`. `session_state()` (`:76`) must change whenever the document changes.
3. **Known capabilities:** add it to `KNOWN_CAPABILITIES` (`methods/mod.rs:35`).
4. **Dispatch:** map the type prefix to the capability in `dispatch` (`methods/mod.rs:130-143`), e.g. `"MaskedEmail" => MASKED_EMAIL`. `requires()` (`api.rs:209`) checks `using`, and `check_account` runs afterwards.
5. **Route the methods:** add `"MaskedEmail/get" => single(masked::get(ctx,&args).await?)`, `"MaskedEmail/set"`, and optionally `/changes` via the shared `changes(ctx,&args,"MaskedEmail",'x')` in `call()` (`mod.rs:167-247`). `/queryChanges` goes through `query_changes.rs`.
6. **New module:** create `methods/masked.rs` and add it to the `mod` list. `senders.rs` is a compact template: `DEFAULTS`, `to_json`, `get` (`get_ids`/`properties`/`pick`/`notFound`), and `set` (`check_set_size`, `if_in_state`, `SetResponse`, `ctx.created_ids.insert`, `SetError::{invalid_properties, not_found, from(StoreError)}`).
7. **IDs:** add a prefix helper in `ids.rs`. Letters already used: a m e t i s l c v u b k r p.
8. **State:** use `ctx.state()` (account modseq) if the store writes `record_change(..., "MaskedEmail", …)`, or a fingerprint like `senders.rs:31`.
9. **Push:** add `"MaskedEmail"` to `push.rs:21` `TYPES` if it should be pushed.
10. **Shared accounts:** leave it out of `sharing.rs:23` `SHARED_METHODS`, so calls on a shared account answer `accountNotSupportedByMethod`.
11. **Tests and docs:** add `tests/integration/masked_email.rs` and register it in `main.rs`; add `docs/jmap-masked-email.md` alongside the existing `docs/jmap-*.md`.

The store layer for masked addresses (new table, resolve and collision hooks) is covered in section 1.
===== aa5f4aff4914e0937
Here is the map for all five areas. Paths are under `/home/user/UwUMail-Server` unless absolute.

Four things matter most for the plan:
- **No background-job framework exists.** Web writes to the DB and a server worker loop picks it up, or a backend trait is plugged into `Web`.
- **Fetched mailboxes already do a per-user "old mail" import**, but only for the inbox and junk folders.
- **There is no OAuth, OIDC, LDAP or `/metrics` code at all.** These appear only as open items in `docs/roadmap.md`.
- **Backup storage is an enum with two variants**, not a trait.

---

## 1. IMAP import, fetched mailboxes, moving from other providers

### `crates/uwumail-server/src/import/imap.rs`
- **`Source`** (L36-47) holds `address`, `tls_name`, `roots`, `master_user: Option<String>`, `password` and `dialer: Option<egress::Dialer>`.
- **`Connection`** (L121-189) is a small client that only speaks implicit TLS.
  - `Connection::open` (L127) connects; `command()` (L171) sends one command.
  - Answers are size-capped by `MAX_LITERAL`, `MAX_LINE` and `MAX_ANSWER` (L19-33).
- **`folders()`** (L270) runs NAMESPACE and LIST, skips shared namespaces, and maps folder roles. Role detection (`role_of`, L242) knows special-use flags plus German and English folder names.
- **`mailbox_for()`** (L334) finds or creates the local mailbox for a folder, parents first.
- **Entry point** (L420):
  ```rust
  pub async fn copy_mail(store: &Store, source: &Source, login: &str, account: &str,
                         dry_run: bool, progress: &mut dyn FnMut(&str)) -> anyhow::Result<Copied>
  ```
  It logs in as `login*master` when a master user is set (L430-434). Then, per folder, it does EXAMINE, reads UIDVALIDITY, and runs `UID SEARCH UID {last+1}:*`. It fetches `UID FETCH … (UID FLAGS INTERNALDATE BODY.PEEK[])` in batches of 25 and stores each message with `store.ingest(IngestRequest{…, keywords, received_at})`.
- **Why it is repeatable:** after each batch it calls `store.set_import_progress(account.id, source_name, folder.raw, ImportProgress{uid_validity, last_uid})` (L517). The table is `import_progress (account_id, source, folder)`; see `crates/uwumail-store/src/import.rs` L9-45 and `migrations/0022_import_progress.sql`. If UIDVALIDITY changed, the folder is copied again from the start.
- **Callers:**
  - `import/mod.rs::imap()` (L14-75) loops over logins and domains, prints with `println!`, and writes an audit entry `import.imap`.
  - The CLI is `cli.rs` L447-470 (`--host --tls-name --master-user --password/UWUMAIL_IMPORT_PASSWORD --login --domain --dry-run`), wired in `main.rs` L115-133.
  - `mailcow.rs` also uses it.

**Can it run per user, with the user's own old-provider password, as a background job from the portal?** The core already works that way: `master_user: None` plus the user's own password is exactly the path `fetch.rs` uses. What is missing compared with fetch:
- **Crate direction.** `import` lives in the binary crate `uwumail-server`. `uwumail-web` cannot call it (web depends only on backup, dav, jmap, smtp and store). Two ways around it:
  - (a) a DB-row job plus a worker in the server, like fetch does, or
  - (b) a backend trait plugged into `Web`, like `HostBackend` (`uwumail-web/src/host.rs` L100) or `GatewayBackend` (`gateway.rs` L118), set through `web.set_host/set_gateway` in `serve.rs` L176-217.
- **Missing safety checks.** No public-address check; `fetch.rs` L173-185 does this (security-audit S-10). No handling of `StoreError::QuotaExceeded`. No duplicate check by Message-ID; `fetch.rs` has `message_key` (L142) and `holds_message`. Its progress callback is a bare `FnMut(&str)`.
- **No time limit.** It runs until it finishes (fetch caps a run at `RUN_LIMIT` 300 s, L46, and continues next time).
- **Credentials.** Keep them sealed if the job must resume after a restart. Reuse `seal`/`unseal` (AES-256-GCM, key kept in the settings table) at `crates/uwumail-store/src/fetch.rs` L269-292.

### Fetched mailboxes: the closest existing background job
- **Worker:** `crates/uwumail-server/src/fetch.rs`.
  - `run_fetchers(store, smtp, egress, shutdown)` (L60) wakes every `TICK` (30 s) and reads `store.fetch_accounts_due()`.
  - Each account runs `run_once` (L157) under `timeout(RUN_LIMIT)`, and the result is written with `store.note_fetch_run(id, fetched, error)`. It is spawned in `serve.rs` L121.
  - `run_once` reuses `import::imap::{Connection, Source, folders, parse_fetch, quoted}` (L36) and goes through the egress dialer.
- **Existing mail ("backlog"):** `take_backlog` (L436) is already a per-user import of old mail. It works in portions of `PER_RUN` (200), stores progress in `FetchFolder{backlog_at, backlog_next, backlog_until}` via `set_fetch_folder`, and skips mail already here (`is_fetch_seen` / `holds_message`).
  - It handles only the inbox and junk folders (`run_once` L221-229); `copy_mail` walks every folder.
  - It is requested with `store.request_fetch_backlog` (store `fetch.rs` L702), which just sets `backlog_at` and clears `last_run_at` so the account is due at once.
  - `run_fetchers` L104-108 reschedules at once while the backlog is still running.
- **Store model:** `crates/uwumail-store/src/fetch.rs`.
  - `FetchAccount` (L131, Serialize, camelCase) has `last_run_at, last_ok_at, last_error, last_fetched, total_fetched, backlog_at`. The portal shows progress from these fields.
  - Also there: `fetch_password` (L404), `fetch_account_due_now` (L685), `finish_fetch_backlog` (L719).
- **Portal routes:** `crates/uwumail-web/src/routes/fetch.rs`.
  - `list`, `discover` (autoconfig through `uwumail_smtp::autoconfig::discover`), `create` (with `take_existing`), `take_existing`, `update`, `delete`, `fetch_now`.
  - Registered in `lib.rs` L332-336.
- **Frontend:** `web/src/features/fetch/FetchPage.tsx`. `backlogAt !== null` shows `fetch.status.takingExisting` (L485).

### Moving calendars from other providers (`dav_import.rs`, commit 48ca957)
- **Route:** `crates/uwumail-web/src/routes/calendar_import.rs::import_remote` (L381-451). It is **synchronous inside the request**, not a background job:
  - `tokio::time::timeout(REMOTE_IMPORT_LIMIT /*300s*/, work)`.
  - Rate limit via `polite()` → `web.allow_remote_call(account_id, 30/h)` (`lib.rs` L239).
  - The password is used once and not kept.
  - It answers with a report per collection.
- **Store side:** `crates/uwumail-store/src/dav_import.rs` (`dav_import`, `dav_mirror`, `DavImportReport`).
- **Frontend:** `web/src/features/calendars/ImportDialogs.tsx` `RemoteDialog` (L311). It shows `calendars.remote.working` while the mutation is pending, and the result when it returns. No polling.
- **What the commit touched:** the route, `web/src/dev/mockApi.ts`, `CalendarsPage.tsx`, `ImportDialogs.tsx`, all 12 locale files, `lib/api.ts` and `lib/errors.ts`. A new portal feature follows the same checklist.

### How the portal shows progress of background work
There are three existing patterns:
1. **DB state plus a worker loop:** fetch, above.
2. **In-memory progress struct plus `tokio::spawn`:**
   - `Backups::start_restore` (`crates/uwumail-backup/src/service.rs` L231-277) claims the slot under a mutex, then `tokio::spawn(this.fetch_restore(...))`.
   - Progress is kept in `Fetching{state: idle|fetching|ready|failed, snapshot, error, started_at, done_bytes, total_bytes}` (L110) and read through `fetching()`.
3. **Frontend polling:**
   - `BackupsPage.tsx` L563-570 polls while a restore is being fetched:
     ```ts
     refetchInterval: (current) => { const data = current.state.data;
       if (data?.restore.fetching.state === "fetching") return 2000;
       return data?.running ? 3000 : false; },
     ```
   - `admin/host.tsx` L33 does the same with `jobBusy(...) ? BUSY_MS : false`.

---

## 2. Backups (`crates/uwumail-backup`)

### How the target is abstracted
The target is **an enum, not a trait**. In `src/storage.rs` L10-13:
```rust
pub enum Storage { Local(PathBuf), Sftp(Sftp) }
```
Its methods are `read(path, limit)`, `write(path, bytes)` (write to `.part`, then rename), `list(dir)`, `remove(path)` and `close()`. Each method matches on the variant.

`Local` already exists and is used by tests and by the CLI restore into a directory.

The configuration type is `sftp::Target{host, port, user, path, login: Login::{Key|Password}, host_key}` (`sftp.rs` L23-45). It is stored inside `BackupSettings{enabled, target: Option<Target>, key, retention, hour, minute}` (`service.rs` L33), as JSON in the settings key `backup.settings`. Status is kept in `backup.status`.

`Backups::open()` (`service.rs` L398-408) is hard-wired to `Sftp::connect(target)` → `Storage::Sftp`. So is `look_at` (L163).

### Adding S3 or a local folder
1. Add `Storage::S3(S3Client)`, and use `Storage::Local` for a mounted folder, with a match arm in each of the five methods.
   - S3 has no rename, so `write` should do a plain PUT (atomic per object).
   - `list("data")` and `list("data/ab")` become prefix listings.
2. Make `Target` an enum:
   ```rust
   #[serde(tag="kind")] enum Target { Sftp{…}, S3{endpoint, bucket, prefix, region, access_key, secret_key}, Local{path} }
   ```
   Keep serde backward compatible: the stored JSON today has no tag. Use `#[serde(untagged)]` or a default.
3. `Backups::open` and `look_at` match on it. `host_key` handling applies only to SFTP.
4. Update `routes/backups.rs` `show/save/test/forget_host_key` (L88-222), `BackupsPage.tsx` `SettingsCard`, and the CLI (`cli.rs` L51-85: `Restore { user@host:/path … }`).

### What a backup and a restore do
- **`backup()`** (`lib.rs` L236-335):
  - The database: `store.snapshot_database` → FastCDC chunks (16/64/256 KiB).
  - Each mail blob as one object, with id = `codec.id_for_hash(sha256)`, so each is uploaded once.
  - The other files in the data directory.
  - Then `Manifest{format, created_at, hostname, version, database: Vec<id>, database_size, blobs: Vec<sha256>, blobs_size, files, uploaded, name}` (`format.rs` L240), written to `snapshots/{now:012}-{rand}`.
  - Then `prune()` (L338) applies `Retention` (`retention.rs`, `keep()` L40) and removes objects no snapshot needs.
- **Encryption:** `format.rs` `Codec` does compression plus AES encryption. Ids are keyed. The `RepoKey` recovery text is 52 characters.
- **Full restore** has two halves:
  - `lib.rs::restore(repo, snapshot, data_dir)` (L375-420) writes the database chunks to `uwumail.db.restoring`, puts blobs at `blobs/ab/cd/<hash>`, writes the other files privately, then renames the database into place. It refuses a directory that already holds a database.
  - Portal half: `start_restore` → `fetch_restore` stages into `<data>/restore/`, writes `restore.ready`, then calls `stop()`.
  - On the next start, `crates/uwumail-server/src/restore.rs::take_over` swaps files by rename before the store opens. The old database is kept as `uwumail.db.replaced`.
  - The CLI restore uses `commands::backup_restore` (`main.rs` L89-92).

### What a per-mailbox restore would need
None of this exists yet; `roadmap.md` lists "Restore per mailbox in the portal, backups to S3 or a mounted folder".
1. Open the repository and read the manifest (`repo.manifest(name)`).
2. Rebuild **only the database** into a temp directory from `manifest.database` chunks. The `repo.get(id, CHUNK_MAX)` loop in `restore()` L391-396 can be reused; `get` is private and needs `pub(crate)` or a new public helper.
3. Open that SQLite file read-only with rusqlite, not `Store::open`, which runs migrations on it. Query `mailboxes` (account_id, name, parent_id, role), `emails` (blob_hash, received_at), `email_mailboxes` and `email_keywords`. Schema: `migrations/0001_initial.sql` L56-135.
4. For each blob hash, `repo.get(&repo.codec.id_for_hash(hash), BLOB_MAX)` and check it with `BlobHash::of`.
5. Store each message in the live store with `store.ingest(IngestRequest{account_id, raw, mailboxes: vec![MailboxTarget::Id(..)], keywords, received_at})`, the same way as `import/imap.rs` L506-513.
   - Duplicates can be skipped with `store.holds_message(account_id, message_id, BlobHash)`.
   - Target either a new "Restored …" folder or the original path. `mailbox_for` from `imap.rs` L334 is the pattern for re-creating the path.
6. Run it in the background with progress, like `Fetching`, and don't let it collide with `running` backups (`is_running()`).
7. `fits_this_server` (`lib.rs` L424) also matters: the schema of an old snapshot may differ.

---

## 3. Authentication

### Portal
- **Password login:** `routes/auth.rs::login` (L78) calls `store.authenticate(login, pw)` (`crates/uwumail-store/src/directory.rs` L738), then `begin_login` (L98).
  - Without a second factor it goes straight to `complete_login` (L121), which does `store.create_web_session` (`store/src/web.rs` L55), records a security event, and sets the cookie.
  - With a second factor, `web.login_state().start(account.id)` hands back a pending token (`uwumail-web/src/login.rs`, kept in memory).
- **Second factor:**
  - `second_factor` (L256) uses `store.check_second_factor_code` (security.rs L946) for TOTP or a recovery code.
  - `passkey_options` / `passkey_login` (L160 / L191) use `webauthn.rs::verify_assertion` and `store.passkey_by_credential`.
- **Session extractors:** `crates/uwumail-web/src/session.rs`.
  - `Session` reads the cookie `__Host-uwumail` (HTTPS) or `uwumail` (plain), calls `store.web_session(token, lifetime)`, and checks the CSRF header `x-csrf-token` for anything other than GET/HEAD.
  - `Admin(Session)` additionally requires `role == Admin`.
  - Cookie names and the 14-day lifetime are defined in `uwumail-jmap/src/auth.rs` L26-30.
- **Security store API** (`crates/uwumail-store/src/security.rs`):
  - App passwords: `AppScope {Mail, Smtp, Dav}` (L64), `create_app_password` (L435), `import_app_password` (L505), `revoke_app_password` (L598).
  - TOTP: `begin_totp` (L871), `confirm_totp` (L904).
  - Passkeys: L1008-1097.
  - Web sessions: L1155-1210.
  - Security events: L1099.
- **Portal routes:** `routes/security.rs`, registered at `lib.rs` L310-326 and L385-386.

### Mail protocols: one central check
**`Store::authenticate_mail(login, password, scope, protocol, ip) -> MailAuth`** (security.rs L618-760):
- It checks app passwords first: a hashed `secret_hash`, then imported hashes.
- Then the main password with argon2, allowed only while `apps_need_app_password` is off and there is no second factor. Otherwise it returns `AppPasswordRequired` and records a `mainPasswordRefused` event.
- It enforces `protocol_allowed`, expiry and scope, and takes the same time for unknown logins.

Where each protocol calls it:
| Protocol | Location | Mechanisms |
|---|---|---|
| IMAP | `crates/uwumail-imap/src/session.rs` L540 `login()` → L546; `authenticate()` L585 | LOGIN and **AUTHENTICATE PLAIN only** (L586 `if mechanism != "PLAIN"`); capabilities L35 `AUTH=PLAIN` |
| SMTP submission | `crates/uwumail-smtp/src/inbound.rs` EHLO L818 `AUTH PLAIN LOGIN`; `auth_plain` L921; `check_credentials` L940 → L944 | PLAIN, LOGIN |
| JMAP / DAV (HTTP) | `crates/uwumail-jmap/src/auth.rs` `Authenticator` | see below |
| ManageSieve | `crates/uwumail-imap/src/managesieve.rs` | STARTTLS, then AUTHENTICATE |

The HTTP `Authenticator` in detail:
- `account()` (L230) handles Basic via `authenticate_mail` (L275), with a 5-minute cache for main-password logins.
- `Bearer <app password>` goes to `bearer()` (L304) → `authenticate_bearer` (security.rs L766).
- A portal session cookie is also accepted, for the webmail (`account_for`, L188).
- Failures are limited per IP / IPv6 /64: 10 per 15 minutes.
- DAV uses `Authenticator::for_protocol(store, AppScope::Dav, "dav")` (`uwumail-dav/src/lib.rs` L75/82).

`POST /jmap/token` exchanges a password (plus TOTP if set) for an app password (`uwumail-jmap/src/token.rs` `handle` L54; `docs/jmap-tokens.md`).

### SASL XOAUTH2 / OAUTHBEARER, OIDC, LDAP
**None of this exists in the code.**
- The only hits are `docs/jmap-clients.md` L48 (an aerc `jmap+oauthbearer://` URL that actually carries an app password as the bearer), `docs/vision.md` L60, and `docs/roadmap.md` L117 ("OAuth 2 / OpenID Connect provider for mail apps; login via external OIDC or LDAP").
- To add them:
  - An OAUTHBEARER/XOAUTH2 branch next to `mechanism != "PLAIN"` in IMAP and in the SMTP `auth` match (inbound.rs ~L880-890).
  - A `Store::authenticate_token`-style check alongside `authenticate_bearer`.
  - Advertising in `CAPABILITIES_BEFORE_LOGIN` and the EHLO lines.

---

## 4. Portal structure

### Registering an API route
Two steps:
1. Add `pub mod x;` in `crates/uwumail-web/src/routes/mod.rs`.
2. Add `.route(...)` in `Web::router()` at `crates/uwumail-web/src/lib.rs` L277-487.

Extractors: `State<Web>`, `Session` or `Admin`, `Path`, `Json`. Errors use `ApiError::Rule("code", detail)`, which becomes HTTP 409 with `{code, detail}` (`error.rs` L22/43). The frontend lists known codes in `web/src/lib/errors.ts` `KNOWN`.

Example handler (`routes/fetch.rs` L193):
```rust
pub async fn take_existing(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().request_fetch_backlog(session.account.id, id).await?;
    Ok(StatusCode::ACCEPTED)
}
```
Its registration (`lib.rs` L336):
```rust
.route("/api/account/fetch/{id}/existing", post(routes::fetch::take_existing))
```

Admin changes are logged with `routes::audit(&web, &session, "action", target, json)` (`routes/mod.rs` L78). Shared state and plugged-in parts live on `Web`/`Inner` (`lib.rs` L63-107), with `OnceLock` fields for `backups`, `host`, `gateway`, `egress` and `dav_transport`.

SPA paths are served in `lib.rs` L489-500: `/account/{*rest}` and `/admin/{*rest}` are already covered.

### Adding a frontend page
- **Router:** `web/src/lib/router.tsx` is a small History-API router (`usePath`, `navigate`, `Link`, `matchPath`).
- **Pages:** `page()` in `web/src/app/App.tsx` L66-100.
  - Account pages use `if (path === "/account/fetch") return <FetchPage />;`.
  - Everything after the `role !== "admin"` check is admin-only.
  - Tabbed admin pages use `*_PATHS` records, e.g. `SERVER_PATHS` in `features/admin/ServerPage.tsx` L11-16: overview, mailFlow, backups, updates, each with `ICONS`, `INTROS` and `admin.tabs.*` labels.
- **Navigation:** `web/src/features/shell/PortalShell.tsx` L210-244, with account items and admin items. `also:` marks tab sub-paths.
- **API types:** `web/src/lib/api.ts` (`api<T>(path, {method, body})` L23, which sends the CSRF token). Types such as `FetchAccountInfo` are at L1123.
- **Mock API:** `web/src/dev/mockApi.ts` must learn the new routes; commit 48ca957 did this.

### i18n
- Files: `web/src/i18n/locales/{de,en,fr,nl,ja,zh}/{neutral,playful}.json`, loaded in `web/src/i18n/index.ts`.
- `playful` overrides only some keys through `fallbackNS: "neutral"`. `useT()` picks the tone, and falls back to neutral when the brand mascot is off.
- `web/src/i18n/locales.test.ts` enforces that every language has exactly the English keys, playful keys exist in neutral, and placeholders are kept.

Neutral example (`en/neutral.json` L2930):
```json
"remote": { "button": "Move from another provider", "title": "Move from another provider", "intro": "Calendars and contacts from iCloud, …", ... }
```
Playful example (`de/playful.json` L153):
```json
"calendars": { "intro": "Teil deine Kalender … (ﾉ◕ヮ◕)ﾉ*:･ﾟ✧", "sharedEmpty": "Noch hat niemand etwas mit dir geteilt (・ω・)" }
```

### Nyu
- `web/src/components/nyu/Nyu.tsx` has `NyuMood` = uwu | sleepy | happy | cheer | sparkle | sad | puzzled.
- `web/src/components/nyu/scenes.tsx` has `NyuScene name=…`, used for example in `LoginPage.tsx` and `ForwardConfirmPage.tsx` L68.
- `HealthCard` picks a mood from the health level.

### Server settings, changed without a restart
- **Allowed keys:** `crates/uwumail-web/src/settings.rs` `SETTINGS` (L44-110), with kinds Bool, Integer, Decimal, Text, Secret, Choice, List.
- **Routes:** `routes/settings.rs` `show`/`update` (L50/90).
  - `merge_changes` validates and refuses keys the config file locks (`settingLocked`).
  - Then `backend.apply(&overlay)`, then `store.set_setting("config.overlay", …)`.
  - `change_settings` (L120) lets other pages change settings, e.g. the VPN page.
- **Applying live:** `crates/uwumail-server/src/settings.rs` `ServerSettings: SettingsBackend::apply` (L65).
  - It reloads `Config::load_with_overlay`, then `smtp.update_settings(...)`, `set_brand`, `loki.set_target`, `egress.reconfigure`, and the `webmail` AtomicBool.
  - The SMTP side reads `Smtp::live()` (`uwumail-smtp/src/lib.rs` L148).
  - At startup the overlay is read in `serve.rs` L61-65.
- **Feature-specific settings** (backups, for example) instead keep their own JSON under a settings key: `store.setting`/`set_setting`, e.g. `backup.settings`.

---

## 5. Admin overview, health, stats, metrics

- **`GET /api/admin/overview`** (`routes/admin.rs` L12) returns `store.server_counts()` (`crates/uwumail-store/src/web.rs` L198-225). That covers domains, accounts, admins, disabled/deleted accounts, aliases, used_bytes, queued_messages, pending_recipients and deferred_recipients, plus hostname, version and uptime.
- **Health:** `GET /api/admin/health` and `POST /api/admin/health/check` go to `crates/uwumail-web/src/health.rs::health()` (L109).
  - Areas: dns, certificate, gateway, delivery, antivirus, storage (disk space, L346-395) and security.
  - Each area is `Area{area, level, findings: Vec<Finding{code, level, params, link}>}`.
  - Background checks run in `Web::run_health_checks` (L418, spawned in `serve.rs` L193); DNS every 6 h, the delivery probe every 1 or 6 h.
- **Frontend:** `web/src/features/admin/AdminHome.tsx` (polls every 30 s), `HealthCard.tsx` (60 s) and `StatusTiles.tsx`.
- **Existing counters and stats:**
  - Egress `AtomicU64`s fetched / failed / proxy_failures / fallbacks (`uwumail-smtp/src/egress.rs` L281-284, shown at `/api/admin/egress`).
  - Spam log (`store/src/spam_log.rs`) and rule hit stats (`migrations/0035_rule_stats.sql`).
  - DMARC/TLS reports (`routes/reports.rs`).
  - Fetch-account totals.
  - The in-memory `LogBuffer` and the Loki shipper (`logs.rs`, `loki.rs`).
- **No `/metrics` endpoint and no Prometheus dependency.** `roadmap.md` L125 lists "Admin alerts, statistics, Prometheus metrics" as open. A metrics route could sit next to `/api/admin/overview` and reuse `server_counts()` plus the health `Level`s.
