# Handover: UwUMail Server 0.14.0 (work in progress)

This was stopped partway through a cloud session. A local agent should take over from here.
**Delete the whole `handoff/` folder before the release PR is merged.**

## Goal (what Lorin asked for)

Build all of this, test it, build it, release it and publish it, with no testing on Lorin's side:

1. A moving assistant in the portal. So far, IMAP import only exists on the command line.
2. Groups, shared mailboxes and masked addresses.
3. Sending our own TLS reports to other domains, and DANE.
4. OAuth2/OIDC provider for mail apps, and portal login via OIDC or LDAP.
5. A calmer admin view, alerts, statistics and Prometheus metrics.
6. Restoring a single mailbox in the portal, and backups to S3 or to a folder.
7. **Web Push** (webmail) and **UnifiedPush** (Android app) through JMAP `PushSubscription`. Lorin decided: build it now, including the Android app.

Roadmap decisions:
- **Remove** "Public test instance" (not needed).
- **Remove** "snooze on the server" (not needed).
- **Remove** "Settings sync for the UwUMail apps": it already exists as JMAP `UserSettings`.
- Tick off everything that is built.
- `docs/vision.md` line 62 ("Much of this is still on the roadmap") and the "calmer view … may come back later" sentence need adjusting.

Release decisions (confirmed by Lorin):
- **Server 0.14.0.**
  - The PR goes from `claude/peaceful-hamilton-3653br` into `main`.
  - Merge it with a merge commit, then push the tag `v0.14.0` on the merge commit.
  - CI then builds the image and the GitHub release (see `handoff/code-map.md`, "A release, step by step").
  - `webmail.pin` points at the webmail merge commit.
- **Webmail.** Merge its PR into `main` first, then pin that commit in the server.
- **Client 0.5.0-beta.4.** Follow `release-notes/README.md` in UwUMail-Client:
  - version bump and `release-notes/0.5.0-beta.4.json`;
  - merge to `main`;
  - wait for the Android and iOS workflows to pass;
  - push the tag `v0.5.0-beta.4`.
- The agent may merge the PRs and push the tags itself.

Files in this folder:
- `plan.md`: the approved plan (in German), with the details of each feature.
- `agent-brief.md`: the rules every feature agent followed. It covers:
  - migration numbers;
  - i18n in all 12 locale files;
  - where to add routes in the router;
  - tests;
  - commit trailers.
- `code-map.md`: a map of the code with paths and line numbers (release process, directory, TLS, auth, backups, portal).

## Where the code is

Each area was built on its own branch, all starting from `186cb72` (0.13.0). Each branch ends in one `wip:` commit that holds the agent's unfinished work. The branches are **not on GitHub**. They are in the bundle `handoff/wip-0.14.bundle`:

```sh
git fetch handoff/wip-0.14.bundle \
  'refs/heads/feat/0.14-*:refs/heads/feat/0.14-*'
git branch --list 'feat/0.14-*'
```

| Branch | Plan section | Migration | Size | Where the agent stopped |
|---|---|---|---|---|
| `feat/0.14-data` | 6 (S3/folder backups, single-mailbox restore) and 1 (moving assistant) | 0040 (not written yet) | 19 files, +2.6k | S3 and folder targets and `mailbox.rs` (restore) written. It was about to write the integration test for mailbox restore. **The moving assistant (section 1) is not started**: no migration 0040, no `migrate.rs`, no portal page. The UI for S3 and restore in `BackupsPage.tsx` is probably missing too. |
| `feat/0.14-directory` | 2 (groups, shared mailboxes, masked addresses) | 0041, 0042 | 68 files, +5.4k | The furthest along. The store is committed (`c257ce0`). The protocol hooks (inbound, submission, JMAP MaskedEmail, principal, sharing), portal pages and i18n are in the wip commit. It was just adding `mockApi.ts` routes. |
| `feat/0.14-tls` | 3 (sending TLS-RPT, DANE) | 0043 | 18 files, +2.2k | `dane.rs`, `tlsrpt.rs`, `store/tls_rpt.rs` and the outbound changes are written. It was working on the integration test `tests/integration/dane.rs`. The DNS-check recommendation for inbound DANE (TLSA 3 1 1, ACME key reuse) and the portal view "Reports we send" are probably still missing. |
| `feat/0.14-auth` | 4 (OAuth provider, OIDC and LDAP login) | 0044 | 30 files, +4.0k | Written: the OAuth store, SASL (OAUTHBEARER/XOAUTH2 in IMAP, SMTP and ManageSieve), JWT, `routes/oauth.rs`, `routes/external_login.rs`. It was about to write `crates/uwumail-web/src/external/` (config, LDAP). **Missing:** the portal UI (consent page, login button, the OAuth list in Security), i18n, docs, tests. |
| `feat/0.14-admin` | 5 (calm view, alerts, statistics, metrics) | 0045 | 41 files, +3.6k | Written: `stats.rs`, `alerts.rs`, `metrics.rs`, the alert texts, `SimpleHome`, `AlertsCard`, `features/stats/`. It was at the stats helpers and their tests. i18n and docs (`docs/metrics.md`) are probably not finished. |
| `feat/0.14-push` | 7, server side (JMAP PushSubscription, RFC 8291/8292) | 0046 | 27 files, +2.7k | Written: the server side (`webpush/`, `push_subscription.rs`, `store/push.rs`) and the test `web_push.rs`. It was writing `docs/jmap-push.md`. |

The other two repositories are on their designated branch `claude/peaceful-hamilton-3653br`, pushed to GitHub, each ending in a `wip:` commit:
- **UwUMail-Webmail.**
  - Done: service worker (`src/sw/`), `src/push/`, `PushSettings.tsx`, i18n.
  - Not finished; checks not run: `pnpm format:check typecheck lint test build audit --prod`.
- **UwUMail-Client.**
  - Done:
    - Core: `jmap_push.rs`, `engine/push_ops.rs`.
    - Android: `UwuPushService.kt`, `Push.kt`, the UnifiedPush connector dependency in `build.gradle.kts`, manifest.
    - Settings: `UnifiedPush.tsx`, only de/en i18n so far.
  - It was at the settings component.
  - Missing: the other languages if the app has them, the release commit 0.5.0-beta.4 (version bump, release notes, roadmap line), and checks.
  - There is no Android SDK locally. Check the APK build with the `android.yml` workflow.

**Nothing has been compiled or tested since the wip commits.** Expect compile errors and failing tests in every branch.

## Next steps

1. Fetch the bundle (see above). Then, per branch:
   - finish what is missing (see the table and `plan.md`);
   - get it building and green, using the CI commands below;
   - squash the `wip:` commit into sensible conventional commits, or leave it with a clear message.
2. **Moving assistant (plan section 1)**. Build it completely; nothing exists yet:
   - migration 0040 `migration_jobs`;
   - worker `crates/uwumail-server/src/migrate.rs`, following the pattern of `fetch.rs` and reusing `import/imap.rs`;
   - the portal page;
   - `docs/moving.md`.
3. Merge the branches one after another into `claude/peaceful-hamilton-3653br`. Expected conflicts:
   - `crates/uwumail-store/src/db.rs` (`MIGRATIONS` list: order 0040…0046);
   - `crates/uwumail-web/src/lib.rs` (router), `routes/mod.rs`, `settings.rs` (`SETTINGS`);
   - `inbound.rs`, `outbound.rs`, `submission.rs`, `serve.rs`, `jmap session.rs` and `methods/mod.rs`;
   - `web/src/dev/mockApi.ts`, `App.tsx`, `PortalShell.tsx`, `lib/api.ts`;
   - the 12 locale JSON files. Check each with `python3 -m json.tool`, then run `pnpm test`, which includes `locales.test.ts`.
4. Run the full CI checks on the merged branch:
   ```sh
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   (cd web && pnpm install --frozen-lockfile && pnpm format:check && pnpm typecheck && pnpm lint && pnpm test && pnpm build)
   ```
   CI also runs `shellcheck` on `*.sh`, `bash deploy/tests/helpers.sh` and `cargo audit`.
5. Docs:
   - `docs/roadmap.md`: remove and tick off as described above;
   - `docs/vision.md`;
   - `CHANGELOG.md` section `## 0.14.0`, in the style of 0.13.0: migrations 0040–0046, an **Updating.** paragraph, a note on the webmail pin;
   - README where features are listed.
6. Release commit `chore(release): 0.14.0`:
   - `Cargo.toml` `[workspace.package] version`, `Cargo.lock`, `webmail.pin`;
   - then the PR "0.14.0: …", merge commit, and the tag `v0.14.0` on the merge commit;
   - CI must be green in the jobs `image` and `release`.
7. Webmail PR and client release as described above.
8. **Remove the `handoff/` folder before merging.**
