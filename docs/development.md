# Development

## Tests

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`crates/uwumail-smtp/tests/flow.rs` starts two servers in the test process
(`a.test` and `b.test`) with self-signed certificates and pinned DNS answers,
and checks submission, DKIM verification across servers, bounces, relay
protection and forged Authentication-Results.

## Web portal

The React app lives in `web/` (pnpm, Node 24):

```bash
cd web
pnpm install
pnpm dev:mock   # every page with sample data, no server needed
pnpm dev        # against a local server's reverse-proxy port (UWUMAIL_DEV_SERVER, default http://127.0.0.1:18080)
pnpm typecheck && pnpm lint && pnpm test
pnpm build      # web/dist, embedded by the next cargo build
```

After the very first `pnpm build`, run `touch crates/uwumail-web/build.rs` so
Cargo notices the new folder; later builds are picked up automatically. With
`pnpm dev:mock`, add `?loggedOut` to the URL to see the login page.

## Local stack

Two containers built from the working tree that deliver to each other:

```bash
docker compose -f dev/compose.yaml up -d --build
bash dev/seed.sh     # domains a.test / b.test, accounts mini, ami, nyu
node dev/smoke.mjs   # portal login, then a mail: delivery and bounce
```

| | a.test | b.test |
| --- | --- | --- |
| SMTP | 127.0.0.1:2525 | 127.0.0.1:4525 |
| Submission (STARTTLS) | 127.0.0.1:2587 | 127.0.0.1:4587 |
| Submission (TLS) | 127.0.0.1:2465 | 127.0.0.1:4465 |
| HTTPS | https://127.0.0.1:8443 | https://127.0.0.1:9443 |

Accounts use the password `katzenpfote-123`. Management commands:

```bash
docker compose -f dev/compose.yaml exec a uwumail-server queue list
docker compose -f dev/compose.yaml logs -f a
```

## Test server and CI

GitHub Actions runs the tests, the web checks and the container image build
(amd64 + arm64) on every push that changes code; `:edge` follows `main`.

To try the current code on a test server before pushing, build the image
locally and copy it over SSH:

```bash
UWUMAIL_DEPLOY_HOST=user@host scripts/deploy-local.sh
UWUMAIL_URL=https://mail.example.com UWUMAIL_LOGIN=… UWUMAIL_PASSWORD_FILE=… node scripts/live-check.mjs
```

## Without Docker

```bash
UWUMAIL_HOSTNAME=localhost UWUMAIL_DATA_DIR=./data UWUMAIL_TLS__MODE=self-signed \
UWUMAIL_LISTEN__SMTP=127.0.0.1:2525 UWUMAIL_LISTEN__SUBMISSION=127.0.0.1:2587 \
UWUMAIL_LISTEN__SUBMISSIONS=127.0.0.1:2465 UWUMAIL_LISTEN__HTTP= UWUMAIL_LISTEN__HTTPS=127.0.0.1:8443 \
cargo run -p uwumail-server -- serve
```

## Conventions

- Code, comments, docs and commit messages in English; user-facing texts in
  German and English with a playful and a neutral tone.
- Conventional commits (`feat(smtp): …`, `fix(store): …`).
- Every protocol feature gets an end-to-end test.
