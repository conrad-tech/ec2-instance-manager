#!/bin/bash
# Run as root on the PRIMARY bastion
# Creates a brand-new user, generates a PEM private key, and installs the derived public key
# With --restore: regenerates the key for a user who already exists, replacing
# their authorized_keys so the key they lost stops working.

set -euo pipefail

usage() {
  echo "Usage: $0 --user <username> [--pem <pem_path>] [--force] [--sudo] [--uid <n>] [--restore] [--help]"
  echo " --user <username> Required. New username to create"
  echo " --pem <pem_path> Optional. PEM output path (default: /root/<username>.pem)"
  echo " --force Optional. Overwrite existing PEM file"
  echo " --sudo Optional. Configure sudo access (NOPASSWD:ALL)"
  echo " --uid <n> Optional. Create with this uid AND gid. Chosen by the app"
  echo "                    from BOTH bastions' tables; without it the script"
  echo "                    picks one this bastion can see (see pick_shared_id)."
  echo " --restore Optional. Restore access for an EXISTING user: require the"
  echo "                    account to exist, replace authorized_keys (revoking"
  echo "                    the lost key) and overwrite any existing PEM."
  echo " --help Show this help message"
}

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "ERROR: Run as root."
  exit 1
fi

USERNAME=""
PEM_PATH=""
FORCE=0
SUDO=0
RESTORE=0
# Explicit id from the caller. Empty means allocate one locally.
WANT_ID=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user)
      USERNAME="${2:-}"
      shift 2
      ;;
    --pem)
      PEM_PATH="${2:-}"
      shift 2
      ;;
    --force)
      FORCE=1
      shift
      ;;
    --sudo)
      SUDO=1
      shift
      ;;
    --uid)
      WANT_ID="${2:-}"
      if [[ ! "$WANT_ID" =~ ^[0-9]+$ ]]; then
        echo "ERROR: --uid needs a number."
        exit 1
      fi
      shift 2
      ;;
    --restore)
      # Restoring implies overwriting the PEM: the original run left one at
      # the default path, and refusing it would fail every restore.
      RESTORE=1
      FORCE=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "ERROR: Unknown option: $1"
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$USERNAME" ]]; then
  echo "ERROR: --user is required."
  usage
  exit 1
fi

if [[ ! "$USERNAME" =~ ^[a-z_][a-z0-9_.-]*[$]?$ ]]; then
  echo "ERROR: Invalid username '$USERNAME'."
  exit 1
fi

if [[ -z "$PEM_PATH" ]]; then
  PEM_PATH="/root/${USERNAME}.pem"
fi

DEFAULT_HOME_DIR="/efs/home/${USERNAME}"
USER_CREATED=0

# A restore never creates anything. Without this a typo'd username would
# quietly produce a new half-configured account instead of telling the
# operator the name was wrong.
if [[ $RESTORE -eq 1 ]] && ! id "$USERNAME" >/dev/null 2>&1; then
  echo "ERROR: User '$USERNAME' does not exist on this bastion."
  echo "Use Bastion New User to create them; --restore only regenerates a key."
  exit 1
fi

if id "$USERNAME" >/dev/null 2>&1; then
  HOME_DIR="$(getent passwd "$USERNAME" | cut -d: -f6)"
  if [[ -z "$HOME_DIR" ]]; then
    HOME_DIR="$DEFAULT_HOME_DIR"
  fi
  echo "User '$USERNAME' already exists. Reusing existing account and home: $HOME_DIR"
else
  HOME_DIR="$DEFAULT_HOME_DIR"
fi

SSH_DIR="${HOME_DIR}/.ssh"
AUTH_KEYS="${SSH_DIR}/authorized_keys"
USER_PEM_PATH="${SSH_DIR}/$(basename "$PEM_PATH")"

if ! command -v ssh-keygen >/dev/null 2>&1; then
  echo "ERROR: ssh-keygen is not available."
  exit 1
fi

mkdir -p /efs/home
chmod 755 /efs /efs/home

if [[ -e "$PEM_PATH" && $FORCE -ne 1 ]]; then
  echo "ERROR: PEM file already exists at $PEM_PATH"
  echo "Use --force to overwrite or provide --pem <new_path>."
  exit 1
fi

mkdir -p "$(dirname "$PEM_PATH")"

echo "Generating PEM private key at $PEM_PATH ..."
ssh-keygen -t rsa -b 4096 -m PEM -N "" -f "$PEM_PATH" -C "${USERNAME}@$(hostname)-$(date +%F)" >/dev/null
chmod 600 "$PEM_PATH"

PUB_KEY="$(ssh-keygen -y -f "$PEM_PATH")"
if [[ -z "$PUB_KEY" ]]; then
  echo "ERROR: Failed to derive public key from PEM."
  exit 1
fi

# One number for both the uid and the gid, free on this bastion and above
# every id already owning something under the shared /efs/home.
#
# Letting useradd and groupadd choose for themselves is what broke: they are
# separate allocators over separate databases, each taking its own lowest free
# number. An account came out uid 1011 / gid 1012 because GID 1011 was spent
# here while UID 1011 was not — and UID 1011 belonged to a different person on
# the secondary. The secondary mirrors these numbers exactly, because EFS
# authorises by number and not by name, so an id already spent over there
# leaves a half-created account and an SSH test that fails.
#
# /efs/home is the one thing both bastions share, which makes the directories
# in it the registry of ids handed out on either box. Ids at or above 60000
# are ignored: root is squashed to nobody (65534) on EFS, and a single file
# owned by it would otherwise push every future account past 65535.
pick_shared_id() {
  local max=999 v
  for v in $(stat -c '%u %g' /efs/home/* 2>/dev/null | tr ' ' '\n' || true); do
    if [[ "$v" =~ ^[0-9]+$ ]] && (( v > max )) && (( v < 60000 )); then
      max=$v
    fi
  done
  local id=$(( max + 1 ))
  while getent passwd "$id" >/dev/null 2>&1 || getent group "$id" >/dev/null 2>&1; do
    id=$(( id + 1 ))
  done
  printf '%s' "$id"
}

# An id passed in beats the local pick: the caller chose it after reading
# BOTH bastions' tables, and this script can only see the one it runs on --
# which is exactly how an account came to hold a number the secondary had
# already spent. The local pick stays for a run by hand.
NEW_ID=""
if ! id "$USERNAME" >/dev/null 2>&1; then
  if [[ -n "$WANT_ID" ]]; then
    NEW_ID="$WANT_ID"
    if getent passwd "$NEW_ID" >/dev/null 2>&1 || getent group "$NEW_ID" >/dev/null 2>&1; then
      echo "ERROR: uid/gid $NEW_ID is already in use on this bastion."
      echo "Run Bastion User Sync: the two bastions disagree about who holds it."
      exit 1
    fi
    echo "Using uid/gid $NEW_ID (chosen from both bastions)."
  else
    NEW_ID="$(pick_shared_id)"
    echo "Allocating uid/gid $NEW_ID (free here, and above every /efs/home owner)."
  fi
fi

# groupadd/useradd report a real failure and stay quiet about the benign
# GROUP=100 / skel / "home already exists" warnings, which come with exit 0.
# Discarding both is what left the secondary saying only that it had failed.
if getent group "$USERNAME" >/dev/null 2>&1; then
  echo "Group '$USERNAME' already exists."
else
  if id "$USERNAME" >/dev/null 2>&1; then
    echo "Skipping group creation because user '$USERNAME' already exists."
  else
    if ! GERR="$(groupadd -g "$NEW_ID" "$USERNAME" 2>&1)"; then
      echo "ERROR: groupadd failed: $GERR"
      exit 1
    fi
  fi
fi

if id "$USERNAME" >/dev/null 2>&1; then
  echo "Skipping user creation because user '$USERNAME' already exists."
else
  if ! UERR="$(useradd -u "$NEW_ID" -m -d "$HOME_DIR" -g "$USERNAME" "$USERNAME" 2>&1)"; then
    echo "ERROR: useradd failed: $UERR"
    exit 1
  fi
  USER_CREATED=1
fi

mkdir -p "$SSH_DIR"
chmod 700 "$SSH_DIR"
touch "$AUTH_KEYS"
chmod 600 "$AUTH_KEYS"

if [[ $RESTORE -eq 1 ]]; then
  # Replace rather than append: the point of a restore is that the previous
  # key is unaccounted for, so leaving it authorized defeats the exercise.
  echo "Replacing authorized_keys for $USERNAME (previous keys revoked)."
  printf '%s\n' "$PUB_KEY" > "$AUTH_KEYS"
elif ! grep -qxF "$PUB_KEY" "$AUTH_KEYS" 2>/dev/null; then
  echo "$PUB_KEY" >> "$AUTH_KEYS"
fi

# Put a user-owned copy of the private key in the user's .ssh directory
cp "$PEM_PATH" "$USER_PEM_PATH"
chmod 600 "$USER_PEM_PATH"

PRIMARY_GROUP="$(id -gn "$USERNAME")"
chown "$USERNAME:$PRIMARY_GROUP" "$SSH_DIR" "$AUTH_KEYS" "$USER_PEM_PATH"

if [[ $USER_CREATED -eq 1 ]]; then
  chown "$USERNAME:$PRIMARY_GROUP" "$HOME_DIR"
fi

if command -v restorecon >/dev/null 2>&1; then
  restorecon -R "$HOME_DIR" || true
fi

if [[ $SUDO -eq 1 ]]; then
  # Convert dots to hyphens for sudoers filename
  SUDOERS_SUFFIX="${USERNAME//./-}"
  SUDOERS_FILE="/etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd"

  echo ""
  echo "Configuring sudo access for $USERNAME..."

  SUDOERS_LINE="$USERNAME ALL=(ALL) NOPASSWD:ALL"
  printf '%s\n' "$SUDOERS_LINE" > "$SUDOERS_FILE"

  # Validate the sudoers file
  if ! visudo -cf "$SUDOERS_FILE"; then
    echo "ERROR: Sudoers file validation failed. Please check $SUDOERS_FILE"
    exit 1
  fi

  # Set proper permissions and ownership
  chmod 0440 "$SUDOERS_FILE"
  chown root:root "$SUDOERS_FILE"

  echo "Sudo access configured successfully at $SUDOERS_FILE"
fi

echo ""
if [[ $USER_CREATED -eq 1 ]]; then
  echo "Created user: $USERNAME"
elif [[ $RESTORE -eq 1 ]]; then
  echo "Restored access for: $USERNAME"
else
  echo "Updated existing user: $USERNAME"
fi
echo "Home: $HOME_DIR"
echo "PEM file (root copy): $PEM_PATH"
echo "PEM file (user copy): $USER_PEM_PATH"
echo ""
echo "Use this key as $USERNAME: --private-key=$USER_PEM_PATH"

# Self-verify AS THE USER. The home is 0700 on EFS and root is squashed to
# 'nobody' there, so root cannot read inside it — only the user can. This
# prints the result right here in the terminal so it's visible.
echo ""
echo "=== Verification (as $USERNAME) ==="
if sudo -n -u "$USERNAME" test -s "$AUTH_KEYS" \
   && sudo -n -u "$USERNAME" test -f "$USER_PEM_PATH"; then
  echo "VERIFY OK: authorized_keys and private key are present."
else
  echo "VERIFY FAILED: expected key files are missing."
fi
sudo -n -u "$USERNAME" ls -la "$SSH_DIR" 2>&1 || \
  echo "(could not list key dir as $USERNAME)"
