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
#
# Everything goes over one SSH connection: some providers refuse connections that come in quick
# succession.
set -euo pipefail

host="${UWUMAIL_GATEWAY_HOST:?set UWUMAIL_GATEWAY_HOST=user@host}"
root="$(cd "$(dirname "$0")/.." && pwd)"

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
if [ "${UWUMAIL_GATEWAY_BUILD:-ci}" = "local" ]; then
  platform="${UWUMAIL_GATEWAY_PLATFORM:-linux/amd64}"
  echo "building the gateway for $platform"
  docker build --platform "$platform" -f "$root/docker/Dockerfile.gateway" --output "type=local,dest=$out" "$root"
  arch="$([ "$platform" = linux/arm64 ] && echo aarch64 || echo x86_64)"
else
  run="${UWUMAIL_GATEWAY_RUN:-$(gh run list --repo MinifyX/UwUMail-Server --workflow CI --branch main \
    --status success --limit 1 --json databaseId --jq '.[0].databaseId')}"
  echo "taking the gateway from CI run $run"
  gh run download "$run" --repo MinifyX/UwUMail-Server --name uwumail-gateway-linux-amd64 --dir "$out"
  (cd "$out" && sha256sum --check --strict uwumail-gateway.sha256)
  arch=x86_64
fi
cp "$root/deploy/gateway/install.sh" "$root/deploy/gateway/gateway.toml" "$root/deploy/gateway/uwumail-gateway.service" "$out/"

echo "installing it on $host"
{
  cat <<REMOTE
set -euo pipefail
if [ "\$(uname -m)" != "$arch" ]; then
  echo "this gateway is built for $arch, the server is \$(uname -m) (see UWUMAIL_GATEWAY_BUILD=local)" >&2
  exit 1
fi
dir=\$(mktemp -d)
trap 'rm -rf "\$dir"' EXIT
base64 -d > "\$dir/gateway.tar" <<'ARCHIVE'
REMOTE
  tar -C "$out" -cf - uwumail-gateway install.sh gateway.toml uwumail-gateway.service | base64
  cat <<'REMOTE'
ARCHIVE
tar -xf "$dir/gateway.tar" -C "$dir"
cd "$dir"
if [ "$(id -u)" -eq 0 ]; then bash install.sh ./uwumail-gateway; else sudo bash install.sh ./uwumail-gateway; fi
REMOTE
} | ssh "$host" bash -s
