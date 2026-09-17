#!/bin/bash
# mailcow-export.sh: writes what UwUMail needs to take over a mailcow installation into one file,
# for `uwumail-server import mailcow`. It only reads: no settings, mail, database rows or containers
# are changed. Mail itself is copied later over IMAP.
#
# Run:    sudo bash mailcow-export.sh [output file]
# Output: /root/uwumail-mailcow-export.jsonl (readable only by root)
#
# The file holds secrets: password hashes, app password hashes, DKIM private keys, calendars and
# contacts. Copy it to the UwUMail server over SSH, import it, then delete it everywhere.
#
# Every line is one JSON object with a "type": domain, aliasDomain, mailbox, alias, appPassword,
# senderAcl, filter, dkim, davFolder or davObject.
set -euo pipefail
export LC_ALL=C

OUT=${1:-/root/uwumail-mailcow-export.jsonl}
[ "$(id -u)" -eq 0 ] || { echo "please run with sudo"; exit 1; }
command -v docker >/dev/null || { echo "docker not found"; exit 1; }

cid() { docker ps -q --filter "label=com.docker.compose.service=$1" | head -n1; }

MC_DIR=$(docker ps --filter "label=com.docker.compose.service=postfix-mailcow" --format '{{.Label "com.docker.compose.project.working_dir"}}' | head -n1)
[ -n "$MC_DIR" ] || MC_DIR=/opt/mailcow-dockerized
[ -f "$MC_DIR/mailcow.conf" ] || { echo "mailcow.conf not found in $MC_DIR"; exit 1; }

conf_value() {
  awk -F= -v k="$1" '$1 == k { sub(/^[^=]*=/, ""); gsub(/^["'\'']|["'\'']$/, ""); print; exit }' "$MC_DIR/mailcow.conf"
}

MYSQL=$(cid mysql-mailcow)
REDIS=$(cid redis-mailcow)
[ -n "$MYSQL" ] || { echo "mysql-mailcow is not running"; exit 1; }
[ -n "$REDIS" ] || { echo "redis-mailcow is not running"; exit 1; }
MYSQL_CLIENT=$(docker exec "$MYSQL" sh -c "command -v mariadb || command -v mysql" | head -n1)

# One JSON object per row. --raw keeps the JSON as it is; JSON_OBJECT escapes newlines itself.
rows() {
  docker exec -e MYSQL_PWD="$(conf_value DBPASS)" "$MYSQL" "$MYSQL_CLIENT" --batch --raw --skip-column-names \
    --user="$(conf_value DBUSER)" --database="$(conf_value DBNAME)" \
    --max-allowed-packet=1G --init-command="SET SESSION TRANSACTION READ ONLY" \
    --execute="$1"
}

redis() {
  local pass
  pass=$(conf_value REDISPASS)
  if [ -n "$pass" ]; then
    docker exec -e REDISCLI_AUTH="$pass" "$REDIS" redis-cli --no-auth-warning --raw "$@"
  else
    docker exec "$REDIS" redis-cli --raw "$@"
  fi
}

# A JSON string from stdin: backslashes, quotes, tabs and line breaks escaped.
json_string() {
  awk 'BEGIN { ORS = "" } { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); gsub(/\t/, "\\t"); gsub(/\r/, "\\r"); if (NR > 1) print "\\n"; print }' |
    { printf '"'; cat; printf '"'; }
}

umask 077
TMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TMP"' EXIT

{
  rows "SELECT JSON_OBJECT('type', 'domain', 'domain', domain, 'active', active) FROM domain"
  rows "SELECT JSON_OBJECT('type', 'aliasDomain', 'aliasDomain', alias_domain, 'targetDomain', target_domain,
          'active', active) FROM alias_domain"
  rows "SELECT JSON_OBJECT('type', 'mailbox', 'username', username, 'name', name, 'password', password,
          'quota', quota, 'active', active, 'domain', domain) FROM mailbox"
  rows "SELECT JSON_OBJECT('type', 'alias', 'address', address, 'goto', goto, 'active', active) FROM alias"
  rows "SELECT JSON_OBJECT('type', 'appPassword', 'mailbox', mailbox, 'name', name, 'password', password,
          'active', active, 'imap', imap_access, 'smtp', smtp_access, 'dav', dav_access) FROM app_passwd"
  rows "SELECT JSON_OBJECT('type', 'senderAcl', 'loggedInAs', logged_in_as, 'sendAs', send_as,
          'external', external) FROM sender_acl"
  rows "SELECT JSON_OBJECT('type', 'filter', 'object', object, 'option', \`option\`, 'value', value) FROM filterconf"
  rows "SELECT JSON_OBJECT('type', 'davFolder', 'id', c_folder_id, 'owner', c_path2, 'path', c_path4,
          'name', c_foldername, 'kind', c_folder_type) FROM sogo_folder_info
        WHERE c_folder_type IN ('Appointment', 'Contact')"
  rows "SELECT JSON_OBJECT('type', 'davObject', 'folder', c_folder_id, 'name', c_name, 'content', c_content)
        FROM sogo_store WHERE c_deleted IS NULL OR c_deleted = 0"

  # DKIM keys live in redis: DKIM_SELECTORS maps a domain to its selector, DKIM_PRIV_KEYS
  # "<selector>.<domain>" to the private key.
  redis HGETALL DKIM_SELECTORS | paste - - | while IFS=$'\t' read -r domain selector; do
    [ -n "$domain" ] || continue
    key=$(redis HGET DKIM_PRIV_KEYS "$selector.$domain")
    [ -n "$key" ] || continue
    printf '{"type":"dkim","domain":%s,"selector":%s,"privateKey":%s}\n' \
      "$(printf '%s' "$domain" | json_string)" "$(printf '%s' "$selector" | json_string)" \
      "$(printf '%s\n' "$key" | json_string)"
  done
} >"$TMP"

mv "$TMP" "$OUT"
trap - EXIT
chmod 600 "$OUT"
echo "Wrote $(wc -l <"$OUT") entries to $OUT ($(du -h "$OUT" | cut -f1))."
echo "It contains password hashes and private keys: copy it over SSH, import it, then delete it."
