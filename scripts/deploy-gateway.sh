#!/usr/bin/env bash
# Builds the UwUMail Gateway on this machine and installs or updates it on a server over SSH.
# Needs Docker here, and a Linux server with systemd there (logged in as root or with sudo).
#
#   UWUMAIL_GATEWAY_HOST=root@gateway.example.com scripts/deploy-gateway.sh
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
echo "building the gateway for $platform"
docker build --platform "$platform" -f "$root/docker/Dockerfile.gateway" --output "type=local,dest=$out" "$root"

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
