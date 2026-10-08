#!/bin/bash
# Restore the keystore (and truststore, when it has a matching backup) saved
# as <store>.bak.<TS> by cassandra.sh. Run once per node via `ssm send-command`
# as root:   cassandra_rollback.sh --restore <TS>
#
# NEVER restarts Cassandra. The app restarts every node at the same moment,
# once every node has been restored.
#
# The current stores are saved as <store>.rollback.<now> BEFORE anything is
# overwritten, so a rollback can itself be undone.
#
# `set -u` only; errexit is deliberately off. Every step reports its own
# outcome as a marker and the verdict is the last marker printed.
set -u
umask 077

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

fail () {
  echo "__CC_RESTORE_FAIL__ $*"
  exit 0
}

[ "${1:-}" = "--restore" ] || fail "usage: cassandra_rollback.sh --restore <TS>"
TS="${2:-}"
# Interpolated into a path: digits only, exactly the shape cassandra.sh writes.
printf '%s' "${TS}" | grep -Eq '^[0-9]{14}$' || fail "invalid backup timestamp '${TS}'"

KEYSTORE_PATH=$(detect_keystore_path) || KEYSTORE_PATH=/etc/cassandra/conf/cassandra-keystore.jks
TRUSTSTORE_PATH="$(dirname "${KEYSTORE_PATH}")/cassandra-truststore.jks"
STORE_PASSWORD=$(detect_store_password) || STORE_PASSWORD=cassandra

KS_BAK="${KEYSTORE_PATH}.bak.${TS}"
TS_BAK="${TRUSTSTORE_PATH}.bak.${TS}"

[ -f "${KS_BAK}" ] || fail "no keystore backup at ${KS_BAK}"
keytool -list -keystore "${KS_BAK}" -storepass "${STORE_PASSWORD}" >/dev/null 2>&1 \
  || fail "keytool cannot open ${KS_BAK} with the node's store password"

if [ -f "${KEYSTORE_PATH}" ]; then
  OWNER=$(stat -c '%U:%G' "${KEYSTORE_PATH}")
else
  OWNER="cassandra:cassandra"
fi

NOW=$(date +%Y%m%d%H%M%S)

# Safety copies first. If either cannot be made, nothing is overwritten.
if [ -f "${KEYSTORE_PATH}" ]; then
  cp -p "${KEYSTORE_PATH}" "${KEYSTORE_PATH}.rollback.${NOW}" \
    || fail "could not save the current keystore as ${KEYSTORE_PATH}.rollback.${NOW}"
fi
if [ -f "${TS_BAK}" ] && [ -f "${TRUSTSTORE_PATH}" ]; then
  cp -p "${TRUSTSTORE_PATH}" "${TRUSTSTORE_PATH}.rollback.${NOW}" \
    || fail "could not save the current truststore as ${TRUSTSTORE_PATH}.rollback.${NOW}"
fi

install -m 600 -o "${OWNER%%:*}" -g "${OWNER##*:}" "${KS_BAK}" "${KEYSTORE_PATH}" \
  || fail "could not install ${KS_BAK} over ${KEYSTORE_PATH}"
if [ -f "${TS_BAK}" ]; then
  install -m 600 -o "${OWNER%%:*}" -g "${OWNER##*:}" "${TS_BAK}" "${TRUSTSTORE_PATH}" \
    || fail "keystore restored but could not install ${TS_BAK} over ${TRUSTSTORE_PATH}"
fi

echo "__CC_RESTORE_OK__ ${TS}"
