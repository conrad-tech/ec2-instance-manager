#!/bin/sh
# Reaper remediation, run once via `ssm send-command` as root.
#
# One shot on purpose. Split across several send-commands there would be a
# window between `down` and `up -d` in which a dropped session or a closed
# GUI leaves reaper stopped *and* its watchdog off. As a single command,
# nothing on the PC side can produce that state.
#
# `set -u` is on, but the exit-on-error flag deliberately is not: turning it
# on would make a failing `down` exit before `up -d` ever runs, which is
# exactly the stranded state above. Every step records its own outcome
# instead, and the verdict is read from `compose ps` rather than from these
# markers -- a 30s timeout can kill an `up -d` that in fact succeeded a
# moment later.
set -u

# `docker ps -a` before anything is touched and again once the stack is back,
# so a human reading the log can see what was actually on the box rather than
# inferring it from the outcome. `-a` on purpose: an exited container is the
# interesting case, and `compose ps` below only speaks for this project.
#
# Capped at 4000 bytes each. `get-command-invocation` truncates
# StandardOutputContent at 24KB and the verdict block is at the *end* of this
# output, so an unbounded listing on a busy box would push the one
# machine-read block off the cap and turn a working fix into Indeterminate.
#
# The markers are deliberately not a superstring of `__RE_PS_BEGIN__`:
# `parse_verdict` requires that marker to appear exactly once, and a snapshot
# block that also matched it would make every run unreadable.
snapshot() {
  echo "__RE_DOCKER_BEGIN__ $1"
  docker ps -a 2>&1 | head -c 4000
  echo
  echo "__RE_DOCKER_END__"
}

echo "__RE_BEGIN__"

# Checked before anything is changed. On the wrong box this must be a no-op,
# not a box left with its self-healing switched off.
if [ ! -d /opt/reaper ]; then
  echo "__RE_NODIR__"
  echo "__RE_END__"
  exit 0
fi

cd /opt/reaper || { echo "__RE_NODIR__"; echo "__RE_END__"; exit 0; }

# Before anything is changed, and after the guard above: on a box with no
# /opt/reaper nothing is touched and nothing is reported about it.
snapshot before-fix

# Left stopped on purpose. The watchdog would race the restart, and a box
# running without it is the reason a *successful* fix is still reported.
if systemctl stop reaper-watchdog 2>&1; then
  echo "__RE_WD_STOPPED__"
else
  echo "__RE_WD_FAIL__"
fi

# stdout only, deliberately -- same reasoning as the `compose ps` block
# below. `get-command-invocation` caps StandardOutputContent at 24KB, and
# these lines are only human context; merging stderr in risks pushing the
# machine-read verdict block past that cap on a large stack. SSM still
# captures stderr separately.
if timeout 30 docker compose down; then
  echo "__RE_DOWN_OK__"
else
  echo "__RE_DOWN_FAIL__"
fi

if timeout 30 docker compose up -d; then
  echo "__RE_UP_OK__"
else
  echo "__RE_UP_FAIL__"
fi

# Straight after the restart. Two more follow at +1m and +5m, sent as their
# own commands by the app -- they cannot live here, since this whole script
# runs under a 90s send-command timeout and sleeping through them would kill
# the verdict block below.
snapshot after-fix

# The authority for the verdict. Everything above is context for a human
# reading the log.
echo "__RE_PS_BEGIN__"
docker compose ps --format json
echo "__RE_PS_END__"

echo "__RE_END__"
