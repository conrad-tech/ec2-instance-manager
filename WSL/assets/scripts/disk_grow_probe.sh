#!/bin/sh
# Reads how big the filesystems and block devices on this box are.
#
# Run by the disk-utilization watcher before and after it grows a volume.
# Every command here is a read: nothing is grown, mounted, removed or written.
# That is enforced by `the_probe_script_changes_nothing_on_the_box`, not by
# review. There is deliberately no stderr redirection either, so the scan can
# refuse any redirection at all.
#
# Markers are assembled at runtime (`s=SEC; echo "__DG_${s}__"`) so that a
# shell echoing the command line can never print a line a parser would accept.
set -u

b=BEGIN; echo "__DG_${b}__"
s=SEC
echo "__DG_${s}__ DFH"
df -h
echo "__DG_${s}__ BYTES"
df -B1 --output=target,size
echo "__DG_${s}__ LSBLK"
lsblk -b -P -o NAME,TYPE,SERIAL,FSTYPE,MOUNTPOINT,PKNAME,SIZE
e=END; echo "__DG_${e}__"
