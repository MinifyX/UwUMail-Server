#!/usr/bin/env bash
# install.sh adds the /host line to a compose.override.yaml somebody wrote themselves. The result
# has to be one docker compose reads with the mount in it, and --remove has to leave their file as
# it was. Runs as any user; the compose check needs docker compose and jq, and is skipped without.
#
#   bash deploy/tests/host-override.sh
set -uo pipefail

awk_file="$(cd "$(dirname "$0")/../host" && pwd)/add-mount.awk"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0
entry=/var/lib/uwumail-host/bridge:/host
printf 'services:\n  uwumail:\n    image: busybox\n  other:\n    image: busybox\n' >"$work/compose.yaml"

check() {
  local name="$1" file="$work/compose.override.yaml" awk_bin
  for awk_bin in mawk gawk; do
    command -v "$awk_bin" >/dev/null || continue
    cp "$work/$name" "$file"
    if ! "$awk_bin" -v svc=uwumail -v entry="$entry" -f "$awk_file" "$file" "$file" >"$file.new" ||
      [ "$(grep -c '# uwumail-host$' "$file.new")" -lt 1 ]; then
      printf 'FAILED  %s (%s): nothing added\n' "$name" "$awk_bin"
      failed=1
      continue
    fi
    if command -v docker >/dev/null && command -v jq >/dev/null && docker compose version >/dev/null 2>&1; then
      cp "$file.new" "$file"
      if ! (cd "$work" && docker compose config --format json 2>/dev/null) |
        jq -e '[.services.uwumail.volumes[]? | select(.target == "/host")] | length > 0' >/dev/null; then
        printf 'FAILED  %s (%s): docker compose does not see the mount\n' "$name" "$awk_bin"
        failed=1
        continue
      fi
    fi
    # What --remove does with a file that is theirs.
    awk -f "$(dirname "$awk_file")/remove-mount.awk" "$file.new" >"$file.back"
    if ! cmp -s "$file.back" "$work/$name"; then
      printf 'FAILED  %s (%s): --remove does not give the file back\n' "$name" "$awk_bin"
      failed=1
      continue
    fi
    printf 'ok      %s (%s)\n' "$name" "$awk_bin"
  done
}

# The prod case: settings but no volumes yet.
cat >"$work/no-volumes" <<'YAML'
services:
  uwumail:
    env_file: env.example
    environment:
      # a comment
      UWUMAIL_LOG__FORMAT: json
YAML
: >"$work/env.example"
# Volumes already there, items at the same indent as the key, another service first.
cat >"$work/compact-list" <<'YAML'
services:
  other:
    image: busybox
  uwumail:   # mine
    volumes:
    - ./data:/data
    ports: ["2525:25"]
YAML
# Four spaces, an indented list.
cat >"$work/wide" <<'YAML'
services:
    uwumail:
        volumes:
            - ./data:/data
        restart: unless-stopped
YAML
for name in no-volumes compact-list wide; do check "$name"; done

# Where it cannot tell, it says so instead of guessing.
printf 'services:\n  uwumail: {image: busybox}\n' >"$work/flow"
if awk -v svc=uwumail -v entry="$entry" -f "$awk_file" "$work/flow" "$work/flow" >/dev/null; then
  printf 'FAILED  flow style: should give up\n'
  failed=1
else
  printf 'ok      flow style gives up\n'
fi

# A service of that name deeper down, or a regex in the name, is not the one.
printf 'services:\n  other:\n    labels:\n      uwumail:\n' >"$work/deeper"
printf 'services:\n  uwumailx:\n    image: busybox\n' >"$work/pattern"
printf 'services:\n  uwumail:\n    volumes: [./a:/a]\n' >"$work/inline"
for name in deeper inline; do
  if awk -v svc=uwumail -v entry="$entry" -f "$awk_file" "$work/$name" "$work/$name" >/dev/null; then
    printf 'FAILED  %s: should give up\n' "$name"
    failed=1
  else
    printf 'ok      %s gives up\n' "$name"
  fi
done
if awk -v svc=uwumai. -v entry="$entry" -f "$awk_file" "$work/pattern" "$work/pattern" >/dev/null; then
  printf 'FAILED  service name taken as a pattern\n'
  failed=1
else
  printf 'ok      service name is a string\n'
fi

# They put an item of their own under the volumes: key install.sh added: --remove keeps the key.
printf 'services:\n  uwumail:\n    volumes:  # uwumail-host\n      - %s  # uwumail-host\n      - ./a:/a\n' "$entry" >"$work/grown"
if [ "$(awk -f "$(dirname "$awk_file")/remove-mount.awk" "$work/grown")" = "$(printf 'services:\n  uwumail:\n    volumes:\n      - ./a:/a')" ]; then
  printf 'ok      --remove keeps their items\n'
else
  printf 'FAILED  --remove orphaned their items\n'
  failed=1
fi

exit "$failed"
