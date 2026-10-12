#!/bin/bash
# Schedules, cancels or shows the one-shot Cassandra restart that follows a cert
# update. Run once via `ssm send-command` as root. The ONLY file it ever writes
# or removes is /etc/cron.d/cassandra-cert-restart.
#
# The cron line is built from the epoch with the node's own `date`, so the
# node's timezone cannot shift the hour. The job removes its own file before
# restarting, so it cannot fire again on the same date next year.
set -u

CRON_FILE=/etc/cron.d/cassandra-cert-restart
MODE=""
EPOCH=""
while [ $# -gt 0 ]; do
  case "$1" in
    --at) MODE=at; EPOCH="${2:-}"; shift 2 ;;
    --cancel) MODE=cancel; shift ;;
    --show) MODE=show; shift ;;
    *) echo "__CC_SCHED_FAIL__ unknown argument $1"; exit 2 ;;
  esac
done

case "${MODE}" in
  show)
    if [ -f "${CRON_FILE}" ]; then
      echo "__CC_SCHED_SET__ $(sed -n 's/^# cassandra-cert-restart //p' "${CRON_FILE}")"
    else
      echo "__CC_SCHED_NONE__"
    fi
    ;;
  cancel)
    rm -f "${CRON_FILE}"
    if [ -e "${CRON_FILE}" ]; then
      echo "__CC_SCHED_FAIL__ could not remove ${CRON_FILE}"
      exit 1
    fi
    echo "__CC_SCHED_NONE__"
    ;;
  at)
    case "${EPOCH}" in
      ''|*[!0-9]*) echo "__CC_SCHED_FAIL__ the time must be epoch seconds"; exit 2 ;;
    esac
    if ! systemctl is-active --quiet cron 2>/dev/null && ! systemctl is-active --quiet crond 2>/dev/null; then
      echo "__CC_SCHED_FAIL__ no active cron daemon (cron or crond)"
      exit 3
    fi
    FIELDS=$(date -d "@${EPOCH}" '+%-M %-H %-d %-m') || { echo "__CC_SCHED_FAIL__ date could not convert ${EPOCH}"; exit 4; }
    printf '# cassandra-cert-restart %s\n%s * root rm -f %s; systemctl restart cassandra\n' \
      "${EPOCH}" "${FIELDS}" "${CRON_FILE}" > "${CRON_FILE}" \
      || { echo "__CC_SCHED_FAIL__ could not write ${CRON_FILE}"; exit 1; }
    chmod 644 "${CRON_FILE}"
    echo "__CC_SCHED_SET__ ${EPOCH}"
    ;;
  *)
    echo "__CC_SCHED_FAIL__ usage: --at EPOCH | --cancel | --show"
    exit 2
    ;;
esac
