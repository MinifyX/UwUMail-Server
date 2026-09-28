#!/usr/bin/env bash
# scripts/deploy-gateway.sh takes a gateway binary that CI built and runs it as root on the VPS. CI
# also runs for pull requests from forks, whose artifacts are built from the fork's code, and such a
# run names the fork's branch -- often `main` (security-audit-0.16.0 GW-3). Only a run for a push to
# main of this repository, of a commit main holds, may ever reach the gateway.
#
# Runs as any user, with jq. `gh` and `ssh` are stand-ins: the first answers from files written
# here, the second only notes that it was called.
#
#   bash deploy/tests/deploy-gateway.sh
set -uo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failed=0

check() {
  local what="$1"
  shift
  if "$@"; then
    printf 'ok      %s\n' "$what"
  else
    printf 'FAILED  %s\n' "$what"
    failed=1
  fi
}

mkdir -p "$work/bin" "$work/gh"
# The GitHub CLI as far as the script uses it. `run list` answers with every run in runs.json,
# whatever it was asked to filter by: the script has to look at each run itself.
cat >"$work/bin/gh" <<'GH'
#!/usr/bin/env bash
data="$FAKE_GH"
expression=.
arguments=("$@")
for ((i = 0; i < ${#arguments[@]}; i++)); do
  case "${arguments[i]}" in
    --jq) expression="${arguments[i + 1]}" ;;
    --dir) dir="${arguments[i + 1]}" ;;
  esac
done
case "$1 $2" in
  "run list") json=$(cat "$data/runs.json") ;;
  "run view") json=$(cat "$data/run-$3.json") || exit 1 ;;
  "run download")
    printf '%s\n' "$3" >"$data/downloaded"
    printf 'a gateway\n' >"$dir/uwumail-gateway"
    (cd "$dir" && sha256sum uwumail-gateway >uwumail-gateway.sha256)
    exit 0
    ;;
  api\ repos/MinifyX/UwUMail-Server/compare/*)
    sha="${2#repos/MinifyX/UwUMail-Server/compare/}"
    json=$(cat "$data/compare-${sha%...main}.json") || exit 1
    ;;
  *) exit 1 ;;
esac
printf '%s' "$json" | jq -r "$expression"
GH
cat >"$work/bin/ssh" <<'SSH'
#!/usr/bin/env bash
cat >/dev/null
printf 'deployed\n' >"$FAKE_GH/ssh"
SSH
chmod +x "$work/bin/gh" "$work/bin/ssh"

on_main=1111111111111111111111111111111111111111
elsewhere=2222222222222222222222222222222222222222
from_fork=3333333333333333333333333333333333333333
run() {
  local id="$1" event="$2" sha="$3"
  printf '{"event":"%s","headBranch":"main","headSha":"%s","conclusion":"success","workflowName":"CI"}' \
    "$event" "$sha" >"$work/gh/run-$id.json"
}
run 30 pull_request "$from_fork"
run 20 push "$on_main"
run 10 push "$elsewhere"
printf '{"status":"ahead"}' >"$work/gh/compare-$on_main.json"
printf '{"status":"diverged"}' >"$work/gh/compare-$elsewhere.json"
printf '{"status":"diverged"}' >"$work/gh/compare-$from_fork.json"

deploy() {
  rm -f "$work/gh/downloaded" "$work/gh/ssh"
  PATH="$work/bin:$PATH" FAKE_GH="$work/gh" UWUMAIL_GATEWAY_HOST=root@gateway.example.com \
    bash "$root/scripts/deploy-gateway.sh" >/dev/null 2>&1
}
took() { [ "$(cat "$work/gh/downloaded" 2>/dev/null)" = "$1" ] && [ -e "$work/gh/ssh" ]; }
refused() { ! deploy && [ ! -e "$work/gh/downloaded" ] && [ ! -e "$work/gh/ssh" ]; }

# The newest run is a fork's pull request whose branch is called main.
printf '[{"databaseId":30},{"databaseId":20},{"databaseId":10}]' >"$work/gh/runs.json"
newest_push() { deploy && took 20; }
check "deploy-gateway: the newest run for a push to main is taken, not a fork's pull request" newest_push

printf '[{"databaseId":30},{"databaseId":10}]' >"$work/gh/runs.json"
check "deploy-gateway: a push whose commit is not on main is not taken" refused

printf '[{"databaseId":20}]' >"$work/gh/runs.json"
named_fork() { UWUMAIL_GATEWAY_RUN=30 refused; }
check "deploy-gateway: a fork's pull request is refused when named" named_fork
named_push() { UWUMAIL_GATEWAY_RUN=20 deploy && took 20; }
check "deploy-gateway: a run for a push to main may be named" named_push
named_garbage() { UWUMAIL_GATEWAY_RUN='20; true' refused; }
check "deploy-gateway: a run is a number" named_garbage

exit "$failed"
