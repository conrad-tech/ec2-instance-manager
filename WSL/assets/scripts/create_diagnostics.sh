U="__USER__"
echo "account : $(getent passwd "$U" 2>/dev/null || echo MISSING)"
echo "group   : $(getent group "$U" 2>/dev/null || echo MISSING)"
sudo -n -u "$U" -H sh -c '
HD="$HOME"
D="$HD/.s""sh"
if [ -d "$HD" ]; then echo "home    : $(ls -ld "$HD")"; else echo "home    : MISSING"; fi
if [ -d "$D" ]; then echo "keydir  : $(ls -ld "$D")"; else echo "keydir  : MISSING"; fi
AK="$D/authorized_keys"
if [ -f "$AK" ]; then echo "authkeys: $(ls -l "$AK") [$(grep -c . "$AK" 2>/dev/null) key line(s)]"; else echo "authkeys: MISSING"; fi
UP="$D/$1.pem"
if [ -f "$UP" ]; then echo "userpem : $(ls -l "$UP")"; else echo "userpem : MISSING"; fi
' _ "$U"
if [ -f "/root/$U.pem" ]; then echo "rootpem : $(ls -l "/root/$U.pem")"; else echo "rootpem : MISSING (root copy; may be normal)"; fi
SF="/etc/sudoers.d/zz-$(echo "$U" | tr '.' '-')-nopasswd"
if [ -f "$SF" ]; then echo "sudoers : $(ls -l "$SF")"; fi
