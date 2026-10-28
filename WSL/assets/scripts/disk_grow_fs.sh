#!/bin/sh
# Grows the partition (when there is one) and the XFS filesystem to fill the
# resized EBS volume. RE_DG_DISK, RE_DG_PART (may be empty) and RE_DG_MOUNT are
# prepended as single-quoted assignments by the caller.
#
# Prints exactly one verdict marker, assembled at runtime so an echoed command
# line cannot fake it: __DG_OK__ followed by `df -h` of the mount, or
# __DG_FAIL__ <command> followed by that command's own output. No marker at all
# is read as a failure by the caller.
set -u

fail() {
  v=FAIL
  echo "__DG_${v}__ $1"
  echo "$2"
  exit 0
}

if [ -n "$RE_DG_PART" ]; then
  n=${RE_DG_PART#"$RE_DG_DISK"}
  n=${n#p}
  o=$(growpart "/dev/$RE_DG_DISK" "$n" 2>&1) || case "$o" in
    NOCHANGE*) ;;
    *) fail growpart "$o" ;;
  esac
fi

o=$(xfs_growfs "$RE_DG_MOUNT" 2>&1) || fail xfs_growfs "$o"
d=$(df -h "$RE_DG_MOUNT" 2>&1)
v=OK
echo "__DG_${v}__"
echo "$d"
