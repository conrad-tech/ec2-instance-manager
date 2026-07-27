echo '__USER__ ALL=(ALL) NOPASSWD:ALL' | tee /etc/sudoers.d/zz-__SUFFIX__-nopasswd > /dev/null
visudo -cf /etc/sudoers.d/zz-__SUFFIX__-nopasswd
chmod 0440 /etc/sudoers.d/zz-__SUFFIX__-nopasswd
chown root:root /etc/sudoers.d/zz-__SUFFIX__-nopasswd
