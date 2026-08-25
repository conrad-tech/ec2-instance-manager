#!/bin/sh
# What reaper_fix.sh looks at, with everything it *changes* removed.
#
# Run by the "Test Alert Match" button against the instance an alert resolves
# to, so the state of the box can be read without taking anything down. Every
# command here is a read: no `compose down`, no `compose up`, no `systemctl
# stop`, nothing written anywhere.
#
# That is the whole contract of this file, and it is enforced by
# `the_probe_script_changes_nothing_on_the_box` rather than by review -- a
# mutating line added here would run unannounced against production from a
# button labelled "test".
set -u

echo "__RE_BEGIN__"

# Listed BEFORE the directory guard, deliberately.
#
# `docker ps -a` does not care where the compose file lives, and the moment
# the guard trips is exactly the moment someone needs to know what is
# actually running on the box -- an unexplained `__RE_NODIR__` with nothing
# beside it is the least useful thing this can report. It is a read, so
# there is nothing to justify gating it behind a check for our directory.
#
# Capped and marked exactly as reaper_fix.sh does it, so one parser reads
# both. See that script for why the cap is there.
echo "__RE_DOCKER_BEGIN__ probe"
docker ps -a 2>&1 | head -c 4000
echo
echo "__RE_DOCKER_END__"

# Exact uptimes, one line per running container.
#
# `docker ps` reports "Up 8 minutes", "Up About a minute", "Up 33 hours
# (healthy)" -- humanised text with special cases, and the difference between
# "About a minute" and 45 seconds is exactly what the caller has to decide
# on. `docker inspect` gives the real start time, so the arithmetic is done
# here against the box's own clock and what crosses is a plain count of
# seconds.
#
# Reads only: `docker ps -q` and `docker inspect` change nothing. A container
# that disappears between the two is skipped rather than failing the run.
__now=$(date -u +%s)
for __c in $(docker ps -q 2>/dev/null); do
  __started=$(docker inspect -f '{{.State.StartedAt}}' "$__c" 2>/dev/null) || continue
  __name=$(docker inspect -f '{{.Name}}' "$__c" 2>/dev/null | sed 's#^/##')
  __begin=$(date -u -d "$__started" +%s 2>/dev/null) || continue
  [ -n "$__name" ] || __name="$__c"
  echo "__RE_UPTIME__ $__name $((__now - __begin))"
done

# Same guard, same marker, same reason as the fix: on a box that is not ours
# this reports and stops. Everything below needs the compose project.
if [ ! -d /opt/cassandra-reaper ]; then
  echo "__RE_NODIR__"
  echo "__RE_END__"
  exit 0
fi

cd /opt/cassandra-reaper || { echo "__RE_NODIR__"; echo "__RE_END__"; exit 0; }

# Read-only: `is-active` reports, it does not start or stop anything. Worth
# having because the fix deliberately leaves the watchdog stopped, so a box
# sitting with it off is the trace of an earlier remediation.
echo "__RE_WD_STATE__ $(systemctl is-active reaper-watchdog 2>&1)"

# stdout only, for the reason reaper_fix.sh gives: compose writes warnings to
# stderr on any subcommand that loads the compose file, and merging them in
# turns the parsed block unreadable.
echo "__RE_PS_BEGIN__"
docker compose ps --format json
echo "__RE_PS_END__"

echo "__RE_END__"
