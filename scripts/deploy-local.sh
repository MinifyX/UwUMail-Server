#!/usr/bin/env bash
# Builds the container image on this machine and copies it straight to a server that runs
# UwUMail with Docker Compose. Needs no registry and no CI minutes, only Docker here and SSH there.
#
#   UWUMAIL_DEPLOY_HOST=user@host [UWUMAIL_DEPLOY_DIR=/opt/uwumail] scripts/deploy-local.sh
#
# On the server the image gets the name its compose file expects
# (ghcr.io/minifyx/uwumail-server:edge), so a plain `docker compose up -d` keeps it.
# A later `docker compose pull` would replace it with the registry's image, which may be older.
set -euo pipefail

host="${UWUMAIL_DEPLOY_HOST:?set UWUMAIL_DEPLOY_HOST=user@host}"
dir="${UWUMAIL_DEPLOY_DIR:-/opt/uwumail}"
image="ghcr.io/minifyx/uwumail-server:${UWUMAIL_VERSION:-edge}"
root="$(cd "$(dirname "$0")/.." && pwd)"
revision="$(git -C "$root" rev-parse --short=7 HEAD)"
if [ -n "$(git -C "$root" status --porcelain)" ]; then
  revision="$revision-dirty"
  echo "warning: uncommitted changes are part of this build" >&2
fi

case "$(ssh "$host" uname -m)" in
  x86_64) platform=linux/amd64 ;;
  aarch64 | arm64) platform=linux/arm64 ;;
  *)
    echo "unknown CPU architecture on $host" >&2
    exit 1
    ;;
esac

echo "building $revision for $platform"
docker build --platform "$platform" -f "$root/docker/Dockerfile" \
  --label "org.opencontainers.image.revision=$revision" \
  -t uwumail-server:deploy "$root"

echo "copying the image to $host"
docker save uwumail-server:deploy | gzip -1 | ssh "$host" docker load

ssh "$host" bash -s -- "$dir" "$image" <<'REMOTE'
set -euo pipefail
cd "$1"
docker tag uwumail-server:deploy "$2"
docker rmi uwumail-server:deploy >/dev/null
docker compose up -d --remove-orphans
for _ in $(seq 1 30); do
  if docker compose exec -T uwumail uwumail-server health 2>/dev/null; then
    echo "running $(docker inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$2")"
    # Earlier UwUMail images that nothing uses any more.
    docker image prune -f --filter "label=org.opencontainers.image.revision" >/dev/null
    echo "healthy (=^･ω･^=)"
    exit 0
  fi
  sleep 2
done
docker compose logs --tail 50 uwumail
echo "not healthy after 60 seconds" >&2
exit 1
REMOTE
