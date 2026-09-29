#!/bin/sh
# One check of the post-fix watch, sent as its own `ssm send-command` every
# 15s for two minutes after each reaper_fix.sh run.
#
# Read-only on purpose -- it is sent up to sixteen times per remediation
# against production. It reports, for each required container, docker's
# status, start time and restart count. The start time and restart count are
# the point: a container that crashes and is restarted between two checks is
# `running` at both, and only those fields show that it went down.
#
# The container names are prepended by the app as RE_WATCH_NAMES, from
# `reaper::REQUIRED_CONTAINERS`, so there is one list and this file carries
# no copy of it. A name docker does not know prints `missing`.
set -u

NAMES="${RE_WATCH_NAMES:-}"
LABEL="${RE_SNAP_LABEL:-watch}"

for __c in $NAMES; do
  __s=$(docker inspect -f '{{.State.Status}} {{.State.StartedAt}} {{.RestartCount}}' "$__c" 2>/dev/null) || __s="missing"
  [ -n "$__s" ] || __s="missing"
  echo "__RE_WATCH__ $__c $__s"
done

# Evidence for the log, logged only when a check finds the stack down.
# Same markers and 4000-byte cap as the other snapshots.
echo "__RE_DOCKER_BEGIN__ $LABEL"
docker ps -a 2>&1 | head -c 4000
echo
echo "__RE_DOCKER_END__"
