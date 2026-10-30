#!/bin/bash
# Compresses old logs under /var/log to free space.
#
# Run by the disk-utilization watcher on every disk alert it handles. It
# gzips regular files in /var/log that were last modified BEFORE today's
# midnight (box time) and are not already compressed. Today's logs are never
# touched. Nothing is deleted or moved: `gunzip` restores any file.
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
echo "__DG_${m}__ files=${files} bytes=${bytes} failed=${failed} dry=${dry}"
