#!/usr/bin/env bash
# Installs or updates the UwUMail Gateway on a server over SSH (a Linux server with systemd, logged
# in as root or with sudo). The binary comes from one of:
#
#   UWUMAIL_GATEWAY_HOST=root@gateway.example.com scripts/deploy-gateway.sh           # CI (default)
#   UWUMAIL_GATEWAY_HOST=... UWUMAIL_GATEWAY_RUN=12345678 scripts/deploy-gateway.sh   # a given CI run
#   UWUMAIL_GATEWAY_HOST=... UWUMAIL_GATEWAY_BUILD=local scripts/deploy-gateway.sh    # Docker here
#
# From CI it takes the "Gateway binary" of the newest successful run on main (needs the GitHub CLI)
# and checks its SHA-256 sum. CI builds only for amd64; arm64 servers need the local build.
set -euo pipefail

host="${UWUMAIL_GATEWAY_HOST:?set UWUMAIL_GATEWAY_HOST=user@host}"
root="$(cd "$(dirname "$0")/.." && pwd)"

case "$(ssh "$host" uname -m)" in
  x86_64) platform=linux/amd64 ;;
  aarch64 | arm64) platform=linux/arm64 ;;
  *)
    echo "unknown CPU architecture on $host" >&2
    exit 1
    ;;
esac

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
if [ "${UWUMAIL_GATEWAY_BUILD:-ci}" = "local" ]; then
  echo "building the gateway for $platform"
  docker build --platform "$platform" -f "$root/docker/Dockerfile.gateway" --output "type=local,dest=$out" "$root"
else
  if [ "$platform" != "linux/amd64" ]; then
    echo "CI builds the gateway for amd64 only; use UWUMAIL_GATEWAY_BUILD=local for $platform" >&2
    exit 1
  fi
  run="${UWUMAIL_GATEWAY_RUN:-$(gh run list --repo MinifyX/UwUMail-Server --workflow CI --branch main \
    --status success --limit 1 --json databaseId --jq '.[0].databaseId')}"
  echo "taking the gateway from CI run $run"
  gh run download "$run" --repo MinifyX/UwUMail-Server --name uwumail-gateway-linux-amd64 --dir "$out"
  (cd "$out" && sha256sum --check --strict uwumail-gateway.sha256)
fi

echo "copying it to $host"
remote=/tmp/uwumail-gateway-install
# shellcheck disable=SC2029 # $remote is a fixed path, meant to expand here.
ssh "$host" "rm -rf $remote && mkdir -p $remote"
scp -q "$out/uwumail-gateway" "$root/deploy/gateway/install.sh" "$root/deploy/gateway/gateway.toml" \
  "$root/deploy/gateway/uwumail-gateway.service" "$host:$remote/"
ssh "$host" bash -s -- "$remote" <<'REMOTE'
set -euo pipefail
cd "$1"
if [ "$(id -u)" -eq 0 ]; then bash install.sh ./uwumail-gateway; else sudo bash install.sh ./uwumail-gateway; fi
cd / && rm -rf "$1"
REMOTE
