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
echo | timeout 10 openssl s_client -connect 127.0.0.1:9142 -servername "${HOST}" 2>/dev/null \
  | openssl x509 -noout -subject -issuer -dates -serial 2>&1
echo "__CC_CERT_END__"
echo "__CC_ACTIVE__ $(systemctl is-active cassandra 2>&1)"
echo "__CC_END__"
