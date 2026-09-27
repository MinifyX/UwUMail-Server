# Common brief for all feature agents (UwUMail Server 0.14.0)

You implement ONE feature area of UwUMail Server 0.14.0 in your own git worktree. Several other agents work in
parallel on other areas in other worktrees; the lead merges all branches afterwards. Read first:
- /home/user/wt/_brief/plan.md  (the approved plan, in German; your section is named in your prompt)
- /home/user/wt/_brief/exploration.md (code map: paths, patterns, line numbers)
- CONTRIBUTING.md, docs/vision.md, docs/architecture.md of the repo

## Rules
- Work ONLY in your worktree directory (given in your prompt) and commit ONLY on its branch (already checked out).
  Never push, never touch other worktrees, never touch /home/user/UwUMail-Server itself.
- Build with the shared target dir: `export CARGO_TARGET_DIR=/home/user/UwUMail-Server/target` for every cargo
  command (other agents build concurrently; waiting on the cargo lock is normal). Use `--locked` except when you
  deliberately add a dependency (then update Cargo.lock with `cargo update -p <crate> --precise` or a normal build,
  and keep the change minimal). Prefer running tests of the crates you touch (`cargo test -p uwumail-store`, …);
  run `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo fmt --all` before each commit.
- Portal (web/): `cd web && pnpm install --frozen-lockfile` once, then `pnpm format:check && pnpm typecheck &&
  pnpm lint && pnpm test && pnpm build` before committing UI work (run `pnpm format` to fix formatting).
- Commits: conventional commits in the repo style (`feat(store): …`, `feat(portal): …`, `test(smtp): …`,
  `docs: …`), several focused commits are welcome. End every commit message with these two lines:
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01AdJVH9bDdoLW87sAxFGdfo
- Style: code, comments and docs in English, matching surrounding code (comment density, naming). Texts users read
  exist in all portal languages: web/src/i18n/locales/{de,en,fr,nl,ja,zh}/neutral.json (every key!) and playful
  overrides where the tone should differ (at least de/en playful for headline texts; locales.test.ts enforces that
  every language has exactly the English neutral keys and that playful keys exist in neutral). Mail texts the server
  writes follow the existing pattern for languages/tones. Examples/tests only use example.com/.org/.net, .test,
  .example, .invalid and documentation IP ranges.
- Security matters: this is a real mail server. Secrets sealed at rest (reuse seal/unseal), constant-time compares,
  rate limits where logins/tokens are checked, SSRF protection for outgoing HTTP (reuse the egress/public-address
  checks like fetch/dav_import do), CSRF (Session extractor), audit entries for admin changes (routes::audit).
- Every new feature needs tests: store unit tests, and end-to-end tests for protocol behaviour (SMTP flow tests in
  crates/uwumail-smtp/tests/integration, JMAP in crates/uwumail-jmap/tests/integration, web routes where there are
  existing route tests). All existing tests must keep passing.
- Migrations: use ONLY the migration number(s) assigned to you; add them to db.rs MIGRATIONS in order (your branch
  may skip numbers of other agents: e.g. if you own 0043 and nobody else is merged yet, name the file
  0043_x.sql but it will be the 40th entry — that is fine, the lead renumbers nothing; positions follow file order
  after merge). Actually to keep it simple: name your file with your assigned number, and append it to MIGRATIONS.
  Update the migration test's table assertions if there are any.
- To reduce merge conflicts in shared files:
  * i18n JSON: add your new top-level sections directly AFTER the existing top-level key named in your prompt
    (not at the end of the file); new keys inside an existing section go at the end of that section.
  * crates/uwumail-web/src/lib.rs router: add your routes as one contiguous block right after the anchor route
    named in your prompt. routes/mod.rs: add your `pub mod` lines in alphabetical position.
  * db.rs MIGRATIONS: append at the end.
  * Do NOT edit CHANGELOG.md, docs/roadmap.md, docs/vision.md, README.md, Cargo.toml version. The lead does that.
- Docs: write/extend a docs/*.md file for your feature in the style of the existing docs (plain, explanatory).
- web/src/dev/mockApi.ts must learn your new routes (the mock drives dev mode and some tests).
- Don't gold-plate, but do finish: the result ships to real users without them testing it. No TODO stubs.

## Final report (your last message)
Return: branch name, list of commits, what you built (short), migrations added, new settings keys, new routes,
anything left undone or risky, and a CHANGELOG paragraph (English, in the style of the 0.13.0 entry quoted in
exploration.md) describing your feature for users.
