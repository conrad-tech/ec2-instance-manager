#!/bin/bash
# Read-only preflight for a rollback, run once per node via `ssm send-command`
# as root. Lists every keystore backup `cassandra.sh` left behind, newest
# first, and says whether each could be restored. Changes nothing.
set -u

# Resolve the keystore this node actually reads out of cassandra.yaml.
detect_keystore_path () {
  local config_file detected
  for config_file in /etc/cassandra/conf/cassandra.yaml /etc/cassandra/cassandra.yaml; do
    if [ -f "${config_file}" ]; then
      detected=$(awk '
        /^client_encryption_options:/ { in_block = 1; next }
        /^[^[:space:]#]/ { in_block = 0 }
        in_block && $1 == "keystore:" { print $2; exit }
      ' "${config_file}")
      if [ -n "${detected}" ]; then
        echo "${detected}"
        return 0
      fi
    fi
  done
  return 1
}

# The configured password wins; `cassandra` is only the fallback.
detect_store_password () {
  local config_file detected
  for config_file in /etc/cassandra/conf/cassandra.yaml /etc/cassandra/cassandra.yaml; do
    if [ -f "${config_file}" ]; then
      detected=$(awk '
        /^client_encryption_options:/ { in_block = 1; next }
        /^[^[:space:]#]/ { in_block = 0 }
        in_block && $1 == "keystore_password:" { print $2; exit }
      ' "${config_file}")
      detected="${detected%\"}"; detected="${detected#\"}"
      detected="${detected%\'}"; detected="${detected#\'}"
      if [ -n "${detected}" ]; then
        echo "${detected}"
        return 0
      fi
    fi
  done
  return 1
}

KEYSTORE_PATH=$(detect_keystore_path) || KEYSTORE_PATH=/etc/cassandra/conf/cassandra-keystore.jks
STORE_PASSWORD=$(detect_store_password) || STORE_PASSWORD=cassandra
KEYSTORE_DIR=$(dirname "${KEYSTORE_PATH}")

# "<not_after_epoch> <SERIAL>" of the first certificate in a keystore.
cert_facts () {
  local out until_text serial epoch
  # LC_ALL=C: the label matching and date parsing must not depend on the node's locale.
  out=$(LC_ALL=C keytool -list -v -keystore "$1" -storepass "${STORE_PASSWORD}" 2>/dev/null) || return 1
  until_text=$(printf '%s\n' "${out}" | awk -F'until: ' '/Valid from:/ { print $2; exit }')
  serial=$(printf '%s\n' "${out}" | awk -F': ' '/Serial number:/ { print toupper($2); exit }')
  [ -n "${until_text}" ] || return 1
  epoch=$(LC_ALL=C date -u -d "${until_text}" +%s 2>/dev/null) || return 1
  echo "${epoch} ${serial:--}"
}

echo "__CC_PF_BEGIN__"
echo "__CC_PF_KEYSTORE__ ${KEYSTORE_PATH}"

# Ownership only: the restore forces mode 600, so a backup's own mode is
# irrelevant; what matters is that it is owned like the live keystore.
LIVE_FACTS=""
[ -f "${KEYSTORE_PATH}" ] && LIVE_FACTS=$(stat -c '%U:%G' "${KEYSTORE_PATH}" 2>/dev/null)

# Newest first: the suffix is YYYYmmddHHMMSS, so reverse lexical order is
# reverse chronological order.
for bak in $(ls -1 "${KEYSTORE_PATH}".bak.* 2>/dev/null | sort -r); do
  ts="${bak##*.bak.}"
  # cassandra_rollback.sh only accepts a 14-digit timestamp; skip e.g. .bak.old
  printf '%s' "${ts}" | grep -Eq '^[0-9]{14}$' || continue
  readable=0; opens=0; perms=0; epoch=0; serial="-"
  [ -r "${bak}" ] && readable=1
  if [ "${readable}" = 1 ]; then
    if facts=$(cert_facts "${bak}"); then
      opens=1
      epoch="${facts%% *}"
      serial="${facts##* }"
    fi
    [ "$(stat -c '%U:%G' "${bak}" 2>/dev/null)" = "${LIVE_FACTS}" ] && perms=1
  fi
  echo "__CC_PF_BACKUP__ ${ts} ${bak} ${epoch} ${serial} ${opens} ${readable} ${perms}"
done

# Room for the .rollback safety copy of the keystore (and truststore).
need_kb=8
for f in "${KEYSTORE_PATH}" "${KEYSTORE_DIR}/cassandra-truststore.jks"; do
  [ -f "${f}" ] && need_kb=$(( need_kb + $(stat -c %s "${f}") / 1024 + 1 ))
done
avail_kb=$(df -Pk "${KEYSTORE_DIR}" 2>/dev/null | awk 'NR==2 { print $4 }')
space=0
[ -n "${avail_kb}" ] && [ "${avail_kb}" -ge "${need_kb}" ] && space=1
echo "__CC_PF_SPACE__ ${space}"
echo "__CC_PF_END__"
