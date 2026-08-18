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

echo "__RE_BEGIN__"

# Checked before anything is changed. On the wrong box this must be a no-op,
# not a box left with its self-healing switched off.
if [ ! -d /opt/reaper ]; then
  echo "__RE_NODIR__"
  echo "__RE_END__"
  exit 0
fi

cd /opt/reaper || { echo "__RE_NODIR__"; echo "__RE_END__"; exit 0; }

# Left stopped on purpose. The watchdog would race the restart, and a box
# running without it is the reason a *successful* fix is still reported.
if systemctl stop reaper-watchdog 2>&1; then
  echo "__RE_WD_STOPPED__"
else
  echo "__RE_WD_FAIL__"
fi

if timeout 30 docker compose down 2>&1; then
  echo "__RE_DOWN_OK__"
else
  echo "__RE_DOWN_FAIL__"
fi

if timeout 30 docker compose up -d 2>&1; then
  echo "__RE_UP_OK__"
else
  echo "__RE_UP_FAIL__"
fi

# The authority for the verdict. Everything above is context for a human
# reading the log.
echo "__RE_PS_BEGIN__"
docker compose ps --format json 2>&1
echo "__RE_PS_END__"

echo "__RE_END__"
