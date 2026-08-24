#!/bin/sh
# One `docker ps -a`, sent as its own `ssm send-command` some minutes after
# the remediation in reaper_fix.sh.
#
# These follow-ups cannot live inside reaper_fix.sh: that script runs under a
# 90s send-command timeout, so sleeping five minutes in it would have the
# invocation cut off before the `compose ps` verdict block was ever read, and
# every fix would come back Indeterminate. The app sends this instead, on its
# own thread, once at +1m and once at +5m.
#
# Read-only on purpose. Nothing here changes the box -- it is evidence about
# whether what the fix started is still running, and it is safe to run on an
# instance whose remediation has already failed.
set -u

# Set by the app, which prepends the assignment before handing the script
# over. The fallback keeps a run by hand readable.
LABEL="${RE_SNAP_LABEL:-snapshot}"

# Same markers and the same 4000-byte cap as reaper_fix.sh's `snapshot`, so
# one parser reads both. See that script for why the cap is there and why
# these markers deliberately do not match `__RE_PS_BEGIN__`.
echo "__RE_DOCKER_BEGIN__ $LABEL"
docker ps -a 2>&1 | head -c 4000
echo
echo "__RE_DOCKER_END__"
