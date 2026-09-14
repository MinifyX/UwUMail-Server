#!/usr/bin/env bash
# Creates the test domains and accounts in the local stack (dev/compose.yaml).
# Safe to run again: existing domains and accounts are left alone.
set -euo pipefail
cd "$(dirname "$0")"

PASSWORD="${UWUMAIL_DEV_PASSWORD:-katzenpfote-123}"

run() {
  local server="$1"
  shift
  docker compose -f compose.yaml exec -T -e UWUMAIL_PASSWORD="$PASSWORD" "$server" uwumail-server "$@"
}

ensure_domain() {
  if ! run "$1" domain list | grep -qx "$2"; then
    run "$1" domain add "$2" >/dev/null
    echo "added domain $2"
  fi
}

ensure_account() {
  local server="$1" address="$2"
  shift 2
  if ! run "$server" account list | grep -q "^$address "; then
    run "$server" account add "$address" "$@" >/dev/null
    echo "added account $address"
  fi
}

ensure_domain a a.test
ensure_account a mini@a.test --name Mini --admin
ensure_account a ami@a.test --name Ami
ensure_domain b b.test
ensure_account b nyu@b.test --name Nyu --admin

echo "ready: log in with any of these addresses and the password $PASSWORD"
