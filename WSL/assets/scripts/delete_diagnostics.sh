U="__USER__"
echo "account : $(getent passwd "$U" 2>/dev/null || echo GONE)"
echo "group   : $(getent group "$U" 2>/dev/null || echo GONE)"
S="$(who 2>/dev/null | awk -v u="$U" '$1==u {print}')"
if [ -n "$S" ]; then echo "sessions: $(echo "$S" | tr '\n' ';')"; else echo "sessions: none"; fi
P="$(pgrep -u "$U" 2>/dev/null | tr '\n' ' ')"
if [ -n "$P" ]; then echo "procs   : $P"; else echo "procs   : none"; fi
if [ -d "/efs/home/$U" ]; then echo "home    : $(ls -ld "/efs/home/$U")"; else echo "home    : removed"; fi
SF="/etc/sudoers.d/zz-$(echo "$U" | tr '.' '-')-nopasswd"
if [ -f "$SF" ]; then echo "sudoers : present ($SF)"; else echo "sudoers : removed"; fi
