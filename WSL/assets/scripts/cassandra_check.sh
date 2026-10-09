#!/bin/bash
# Read-only: what this Cassandra node is serving, and whether it is running.
# Run once via `ssm send-command` as root. Changes nothing on the box.
#
# The same openssl pipeline an operator runs by hand against the native
# transport TLS port (9142), plus -serial so a renewal can be told apart from
# the cert it replaced even when the expiry date is unchanged.
set -u

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
echo "__CC_CERT_END__"
echo "__CC_ACTIVE__ $(systemctl is-active cassandra 2>&1)"
echo "__CC_END__"
