# Dump this bastion's account table for the cross-bastion comparison.
#
# ACCT is the managed set: an account is "managed" when its home is under the
# shared /efs/home, which is what makes its numbers have to agree on both
# boxes. ssm-user, ec2-user and the system accounts live elsewhere and are
# each box's own business.
#
# UIDU/GIDU are deliberately unfiltered: a number spent by a *system* account
# still blocks a useradd, so the conflict report needs the whole table, not
# just the managed part.
awk -F: '$6 ~ /^\/efs\/home\// && $3 >= 1000 && $3 < 60000 {print "ACCT", $1, $3, $4, $6}' /etc/passwd
awk -F: '{print "UIDU", $3, $1}' /etc/passwd
awk -F: '{print "GIDU", $3, $1}' /etc/group
# Every directory on the shared mount, by name and owner. THIS IS THE
# AUTHORITY: the files already carry the numbers that matter, both boxes see
# the same ones, and they are the thing being protected. An account is
# realigned onto its home rather than the home onto the account, because that
# direction chowns nothing.
#
# A home can also outlive its account -- deleted on both boxes, the directory
# stays, owned by a number no passwd entry mentions. Handing that number to
# somebody new would give them the files, so the allocator floors above these.
for d in /efs/home/*/; do [ -d "$d" ] || continue; n="${d%/}"; n="${n##*/}"; stat -c "HOMEOWN $n %u %g" "$d"; done 2>/dev/null
