#!/bin/bash
# Frees space on a box that is filling up.
#
# Run by the disk-utilization watcher on every disk alert it handles, and by
# the Scripts-menu Disk Utilization Check. In this order:
#   1. truncate docker container json logs
#   2. vacuum the systemd journal to 2 days
#   3. delete /tmp files not accessed for 10 days
#   4. truncate *.log files under /var/log not modified for 7 days
#   5. gzip what is left in /var/log that was last modified BEFORE today's
#      midnight (box time) and is not already compressed. `gunzip` restores
#      a gzipped file; steps 1-4 do not keep what they empty or remove.
# Today's logs are never gzipped.
#
# RE_DG_DRY=1 (set by Test Alert Match) only counts: no file is changed.
#
# The verdict is one marker line, assembled at runtime so that a shell echoing
# the command line can never print a line the parser would accept.
set -u

dry="${RE_DG_DRY:-0}"
today="$(date +%Y-%m-%d)"
files=0
bytes=0
failed=0
cleaned=0
cleaned_bytes=0
journal=none

# Empties (truncate) or removes (delete) each NUL-separated path on stdin,
# counting what it did and the bytes it held. Dry only counts.
sweep() {
  local f size
  while IFS= read -r -d '' f; do
    size="$(stat -c %s -- "$f")" || size=0
    if [ "$dry" = "1" ]; then
      cleaned=$((cleaned + 1))
      cleaned_bytes=$((cleaned_bytes + size))
    elif { [ "$1" = "truncate" ] && truncate -s 0 -- "$f"; } || { [ "$1" = "delete" ] && rm -f -- "$f"; }; then
      cleaned=$((cleaned + 1))
      cleaned_bytes=$((cleaned_bytes + size))
    else
      failed=$((failed + 1))
    fi
  done
}

# 1. docker container logs (no -xdev: docker is often on its own volume)
if [ -d /var/lib/docker/containers ]; then
  find /var/lib/docker/containers -type f -name '*-json.log' -size +0 -print0 | sweep truncate
fi

# 2. systemd journal
if command -v journalctl >/dev/null 2>&1; then
  if [ "$dry" = "1" ]; then
    journal=skipped
  elif journalctl --vacuum-time=2d >/dev/null 2>&1; then
    journal=ok
  else
    journal=failed
  fi
fi

# 3. old temp files
find /tmp -type f -atime +10 -print0 | sweep delete

# 4. old log files
find /var/log -type f -name '*.log' -size +0 -mtime +7 -print0 | sweep truncate

while IFS= read -r -d '' f; do
  size="$(stat -c %s -- "$f")" || size=0
  if [ "$dry" = "1" ]; then
    files=$((files + 1))
    bytes=$((bytes + size))
  elif nice -n 19 gzip -n -- "$f"; then
    files=$((files + 1))
    bytes=$((bytes + size))
  else
    failed=$((failed + 1))
  fi
done < <(find /var/log -xdev -type f -size +0 ! -newermt "$today" \
  ! -name '*.gz' ! -name '*.xz' ! -name '*.bz2' ! -name '*.zst' ! -name '*.zip' \
  ! -name 'wtmp*' ! -name 'btmp*' ! -name 'lastlog' ! -name 'faillog' \
  ! -path '/var/log/journal/*' -print0)

m=GZ
echo "__DG_${m}__ files=${files} bytes=${bytes} failed=${failed} dry=${dry} cleaned=${cleaned} cleaned_bytes=${cleaned_bytes} journal=${journal}"
