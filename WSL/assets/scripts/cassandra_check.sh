#!/bin/bash
# Read-only: what this Cassandra node is serving, and whether it is running.
# Run once via `ssm send-command` as root. Changes nothing on the box.
#
# The same openssl pipeline an operator runs by hand against the native
# transport TLS port (9142), plus -serial so a renewal can be told apart from
# the cert it replaced even when the expiry date is unchanged.
set -u

# The keystore this node actually reads, out of cassandra.yaml (the same lookup
# the renew and rollback scripts use), so its modified time can say when the
# cert was last installed here.
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

HOST=$(hostname -f 2>/dev/null || hostname)

echo "__CC_BEGIN__"
echo "__CC_HOST__ ${HOST}"
echo "__CC_CERT_BEGIN__"
# Cassandra's native transport is bound to the node's address (rpc_address), not
# always loopback, so a connect to 127.0.0.1 can handshake with nothing. Try the
# name an operator uses by hand first, then the node's own IP, then loopback;
# the first that serves a cert wins, and the last attempt's output is kept when
# none does.
IP=$(hostname -I 2>/dev/null | awk '{print $1}')
CERT_OUT=""
for target in "${HOST}" "${IP}" 127.0.0.1; do
  [ -n "${target}" ] || continue
  CERT_OUT=$(echo | timeout 10 openssl s_client -connect "${target}:9142" -servername "${HOST}" 2>/dev/null \
    | openssl x509 -noout -subject -issuer -dates -serial 2>&1)
  case "${CERT_OUT}" in subject*) break ;; esac
done
echo "${CERT_OUT}"
# No cert came back: show whether Cassandra is even installed. A missing unit
# prints "Unit cassandra.service could not be found." This runs as root, so no
# sudo is needed.
case "${CERT_OUT}" in
  subject*) ;;
  *)
    echo "--- systemctl status cassandra ---"
    systemctl status cassandra --no-pager 2>&1 | head -n 15
    ;;
esac
echo "__CC_CERT_END__"
KEYSTORE_PATH=$(detect_keystore_path) || KEYSTORE_PATH=/etc/cassandra/conf/cassandra-keystore.jks
echo "__CC_KEYSTORE_MTIME__ $(stat -c %Y "${KEYSTORE_PATH}" 2>/dev/null)"
echo "__CC_ACTIVE__ $(systemctl is-active cassandra 2>&1)"
echo "__CC_END__"
