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
# Only a run of this repository's CI for a push to main is taken, and only while its commit is part
# of main. CI also runs for pull requests from forks, and such a run reports the fork's branch name
# -- `main` as often as not -- while its artifact is built from whatever the fork contains. The
# binary ends up running as root on the gateway (security-audit-0.16.0 GW-3).
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
  repo=MinifyX/UwUMail-Server
  # Whether run $1 is this repository's CI, successful, for a push to main, of a commit main still
  # holds. The event is what tells a push to this repository from a fork's pull request; the
  # comparison with main is asked of GitHub rather than of a local clone that may be out of date.
  from_main() {
    local run="$1" sha status
    [[ "$run" =~ ^[0-9]{1,20}$ ]] || return 1
    sha=$(gh run view "$run" --repo "$repo" --json event,headBranch,headSha,conclusion,workflowName --jq \
      'select(.event == "push" and .headBranch == "main" and .conclusion == "success" and .workflowName == "CI") | .headSha') ||
      return 1
    [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || return 1
    status=$(gh api "repos/$repo/compare/$sha...main" --jq .status) || return 1
    [ "$status" = identical ] || [ "$status" = ahead ]
  }
  if [ -n "${UWUMAIL_GATEWAY_RUN:-}" ]; then
    run="$UWUMAIL_GATEWAY_RUN"
    from_main "$run" || {
      echo "CI run $run is not a successful run for a push to main of $repo; not taking its gateway" >&2
      exit 1
    }
  else
    run=""
    for candidate in $(gh run list --repo "$repo" --workflow CI --branch main --event push --status success \
      --limit 20 --json databaseId --jq '.[].databaseId'); do
      if from_main "$candidate"; then
        run="$candidate"
        break
      fi
    done
    [ -n "$run" ] || {
      echo "found no successful CI run for a push to main of $repo" >&2
      exit 1
    }
  fi
  echo "taking the gateway from CI run $run"
  gh run download "$run" --repo "$repo" --name uwumail-gateway-linux-amd64 --dir "$out"
  (cd "$out" && sha256sum --check --strict uwumail-gateway.sha256)
  arch=x86_64
fi
cp "$root/deploy/gateway/install.sh" "$root/deploy/gateway/gateway.toml" "$root/deploy/gateway/uwumail-gateway.service" "$out/"
# install.sh looks after the machine as well; everything it needs for that lives in here.
cp -r "$root/deploy/gateway/hardening" "$out/"

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
  tar -C "$out" -cf - uwumail-gateway install.sh gateway.toml uwumail-gateway.service hardening | base64
  cat <<'REMOTE'
ARCHIVE
tar -xf "$dir/gateway.tar" -C "$dir"
cd "$dir"
if [ "$(id -u)" -eq 0 ]; then bash install.sh ./uwumail-gateway; else sudo bash install.sh ./uwumail-gateway; fi
REMOTE
} | ssh "$host" bash -s
