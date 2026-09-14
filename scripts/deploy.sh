#!/usr/bin/env bash
# Updates a server that runs UwUMail with Docker Compose to the newest image and
# waits until it is healthy.
#
#   UWUMAIL_DEPLOY_HOST=user@host [UWUMAIL_DEPLOY_DIR=/opt/uwumail] scripts/deploy.sh [tag]
set -euo pipefail

host="${UWUMAIL_DEPLOY_HOST:?set UWUMAIL_DEPLOY_HOST=user@host}"
dir="${UWUMAIL_DEPLOY_DIR:-/opt/uwumail}"
tag="${1:-edge}"

ssh "$host" bash -s -- "$dir" "$tag" <<'REMOTE'
set -euo pipefail
cd "$1"
export UWUMAIL_VERSION="$2"
docker compose pull --quiet
docker compose up -d --remove-orphans
for _ in $(seq 1 30); do
  if docker compose exec -T uwumail uwumail-server health 2>/dev/null; then
    docker compose images uwumail
    echo "healthy (=^･ω･^=)"
    exit 0
  fi
  sleep 2
done
docker compose logs --tail 50 uwumail
echo "not healthy after 60 seconds" >&2
exit 1
REMOTE
