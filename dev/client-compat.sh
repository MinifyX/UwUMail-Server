#!/usr/bin/env bash
# Runs the UwUMail app's JMAP integration test against this server.
#
#   UWUMAIL_CLIENT=../UwUMail bash dev/client-compat.sh
#
# Starts a throwaway server (data in a temp directory) with the accounts the
# app's test expects, runs `cargo test -p uwumail-core --test stalwart` in the
# app repository, and stops the server again.
set -euo pipefail
cd "$(dirname "$0")/.."

CLIENT="${UWUMAIL_CLIENT:-../UwUMail}"
PORT="${UWUMAIL_COMPAT_PORT:-18080}"
WORK="$(mktemp -d)"
trap 'kill "${SERVER:-0}" 2>/dev/null || true; rm -rf "$WORK"' EXIT

cargo build -q -p uwumail-server
BIN="target/debug/uwumail-server"
[ -x "$BIN" ] || BIN="$BIN.exe"

export UWUMAIL_HOSTNAME=mail.uwumail.test
export UWUMAIL_DATA_DIR="$WORK/data"
export UWUMAIL_TLS__MODE=self-signed
export UWUMAIL_LISTEN__SMTP=127.0.0.1:12525
export UWUMAIL_LISTEN__SUBMISSION=127.0.0.1:12587
export UWUMAIL_LISTEN__SUBMISSIONS=
export UWUMAIL_LISTEN__HTTP=
export UWUMAIL_LISTEN__HTTPS=
export UWUMAIL_LISTEN__PROXY="127.0.0.1:$PORT"
export UWUMAIL_SMTP__VERIFY_SENDERS=false

"$BIN" domain add uwumail.test >/dev/null
UWUMAIL_PASSWORD='Kirschbluete-Tastatur-42!' "$BIN" account add mini@uwumail.test --name Mini >/dev/null
UWUMAIL_PASSWORD='Seifenblase-Wanderweg-17!' "$BIN" account add leni@uwumail.test --name "Leni Wanders" >/dev/null

"$BIN" serve >"$WORK/server.log" 2>&1 &
SERVER=$!
for _ in $(seq 1 50); do
  curl -fs -o /dev/null "http://127.0.0.1:$PORT/healthz" && break
  sleep 0.2
done

(cd "$CLIENT" && UWUMAIL_TEST_JMAP="http://127.0.0.1:$PORT" cargo test -p uwumail-core --test stalwart) || {
  echo "--- server log ---" >&2
  tail -50 "$WORK/server.log" >&2
  exit 1
}
echo "The UwUMail app works with this server (=^･ω･^=)"
