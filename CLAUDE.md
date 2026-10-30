# UwUMail-Server

Selbst gehosteter Mailserver (SMTP/IMAP/JMAP/CalDAV/CardDAV) mit Web-Portal, Rust-Workspace v0.25.x, Edition 2024.

## Layout
- `crates/uwumail-server`: Binary, verdrahtet alles; `-smtp`, `-imap`, `-jmap`, `-dav`: Protokolle; `-tnef`: winmail.dat-Parser
- `crates/uwumail-store`: Speicher (rusqlite); `-web`: Portal-API, bettet `web/dist` ein (`build.rs`)
- `crates/uwumail-assist` (KI-Assistent), `-labels`, `-backup`, `-gateway` + `-tunnel` (Gateway, siehe `docs/gateway.md`)
- `web/`: Vite/React-Portal (pnpm); `docker/`, `deploy/`, `compose.yaml`: Images und Betrieb
- `dev/`: lokaler Zwei-Server-Stack (`compose.yaml`, `seed.sh`, `smoke.mjs`, `client-compat.sh`)

## Befehle
- Portal ohne Backend: `pnpm --dir web dev:mock` (`.claude/launch.json`, Port 5174)
- Lokaler Smoke-Test: `dev/seed.sh`, dann `node dev/smoke.mjs`
- Windows: Tests mit `-- --test-threads=1` (aws-lc-Keygen stürzt sonst ab, siehe `docs/development.md`)

## Fallen
- `web/dist` wird per `build.rs` ins Binary eingebettet (oder `UWUMAIL_WEB_DIST`): nach Portal-Änderung erst `pnpm build`, dann Cargo; nie parallel zu `cargo test`.
- `webmail.pin`: Webmail kommt aus eigenem Repo, per vollem Commit-Hash gepinnt (nie Branch). Pin nur beim Release bewusst anheben, Webmail-Release zuerst.
- `Cargo.lock`, `web/pnpm-lock.yaml`, `web/dist/` nie von Hand ändern (CI nutzt `--locked`).
- Neue Tests in die bestehende Integrationstest-Binary der Crates einhängen (siehe ~/Projects/CLAUDE.md).

## Plan/Stand
- Einstieg: `docs/architecture.md`, `docs/development.md`, `docs/roadmap.md`, `CHANGELOG.md`; Features je Thema in `docs/*.md`.

## Release
- Version in `Cargo.toml` (`workspace.package.version`), neuer Abschnitt `## <version>` in `CHANGELOG.md` (CI übernimmt ihn ins GitHub-Release), Pin in `webmail.pin` prüfen, Tag `vX` auf Merge-Commit.
- Zusätzlich Security-Audit-Dokument (siehe unten).

## Security
- Kritische Bereiche: Netzwerk-Parser (`uwumail-smtp`, `-imap`, `-jmap`, `-dav`, `-tnef`), Auth/OAuth/OIDC/LDAP (`uwumail-web`, `docs/oauth.md`, `docs/login-oidc-ldap.md`), Egress-Proxy, `uwumail-gateway`/`-tunnel`, Backup, Passwort-/Token-Handling in `uwumail-store`.
- Vor jedem Feature-Release den globalen Subagent `security-reviewer` über den Bereich seit dem letzten Tag laufen lassen; Ergebnis nach `docs/security-audit-<version>.md` (bestehendes Schema; Scope per `git diff main...release/<version>`).
