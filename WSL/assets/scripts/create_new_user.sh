#!/bin/bash
# Run as root on the PRIMARY bastion
# Creates a brand-new user, generates a PEM private key, and installs the derived public key

set -euo pipefail

usage() {
  echo "Usage: $0 --user <username> [--pem <pem_path>] [--force] [--sudo] [--help]"
  echo " --user <username> Required. New username to create"
  echo " --pem <pem_path> Optional. PEM output path (default: /root/<username>.pem)"
  echo " --force Optional. Overwrite existing PEM file"
  echo " --sudo Optional. Configure sudo access (NOPASSWD:ALL)"
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

if getent group "$USERNAME" >/dev/null 2>&1; then
  echo "Group '$USERNAME' already exists."
else
  if id "$USERNAME" >/dev/null 2>&1; then
    echo "Skipping group creation because user '$USERNAME' already exists."
  else
    groupadd "$USERNAME"
  fi
fi

if id "$USERNAME" >/dev/null 2>&1; then
  echo "Skipping user creation because user '$USERNAME' already exists."
else
  useradd -m -d "$HOME_DIR" -g "$USERNAME" "$USERNAME"
  USER_CREATED=1
fi

mkdir -p "$SSH_DIR"
chmod 700 "$SSH_DIR"
touch "$AUTH_KEYS"
chmod 600 "$AUTH_KEYS"

if ! grep -qxF "$PUB_KEY" "$AUTH_KEYS" 2>/dev/null; then
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
else
  echo "Updated existing user: $USERNAME"
fi
echo "Home: $HOME_DIR"
echo "PEM file (root copy): $PEM_PATH"
echo "PEM file (user copy): $USER_PEM_PATH"
echo ""
echo "Run this on the secondary bastion:"
echo "UID_NUM=\$(stat -c %u /efs/home/$USERNAME)"
echo "GID_NUM=\$(stat -c %g /efs/home/$USERNAME)"
echo "sudo groupadd -g \$GID_NUM $USERNAME"
echo "sudo useradd -u \$UID_NUM -g \$GID_NUM -d /efs/home/$USERNAME $USERNAME"
if [[ $SUDO -eq 1 ]]; then
  SUDOERS_SUFFIX="${USERNAME//./-}"
  echo "echo '$USERNAME ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd > /dev/null"
  echo "sudo visudo -cf /etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd"
  echo "sudo chmod 0440 /etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd"
  echo "sudo chown root:root /etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd"
fi
echo ""
echo "Use this key as $USERNAME: --private-key=$USER_PEM_PATH"
