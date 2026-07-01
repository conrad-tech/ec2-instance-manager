#!/bin/bash
# Run as root. Deletes a user previously created by create_new_user.sh.
# Best-effort cleanup: continues past individual failures.
#
#   --user <username>   Required. User to delete.
#   --remove-home       Also remove the shared home /efs/home/<username>.
#                       Only honoured on the primary (ignored with --secondary).
#   --secondary         Secondary-bastion mode: remove the local account,
#                       group and sudoers entry only. Leaves the shared EFS
#                       home and the root PEM copy to the primary run.

set -uo pipefail

USERNAME=""
REMOVE_HOME=0
SECONDARY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --user)
      USERNAME="${2:-}"
      shift 2
      ;;
    --remove-home)
      REMOVE_HOME=1
      shift
      ;;
    --secondary)
      SECONDARY=1
      shift
      ;;
    --help|-h)
      echo "Usage: $0 --user <username> [--remove-home] [--secondary]"
      exit 0
      ;;
    *)
      echo "ERROR: Unknown option: $1"
      exit 1
      ;;
  esac
done

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "ERROR: Run as root."
  exit 1
fi

if [[ -z "$USERNAME" ]]; then
  echo "ERROR: --user is required."
  exit 1
fi

if [[ ! "$USERNAME" =~ ^[a-z_][a-z0-9_.-]*[$]?$ ]]; then
  echo "ERROR: Invalid username '$USERNAME'."
  exit 1
fi

echo "Deleting user '$USERNAME'..."

if id "$USERNAME" >/dev/null 2>&1; then
  # Refuse to delete a user who is currently active — either a logged-in
  # session or any running process. We do NOT kill their sessions; the
  # admin should ask them to log out and re-run.
  ACTIVE_SESSIONS="$(who 2>/dev/null | awk -v u="$USERNAME" '$1==u {print}')"
  ACTIVE_PROCS="$(pgrep -u "$USERNAME" 2>/dev/null | tr '\n' ' ' | sed 's/ *$//')"
  if [[ -n "$ACTIVE_SESSIONS" || -n "$ACTIVE_PROCS" ]]; then
    echo "ABORTED: user '$USERNAME' is currently active on $(hostname); not deleting."
    if [[ -n "$ACTIVE_SESSIONS" ]]; then
      echo "Active login sessions:"
      echo "$ACTIVE_SESSIONS"
    fi
    if [[ -n "$ACTIVE_PROCS" ]]; then
      echo "Running process PIDs: $ACTIVE_PROCS"
    fi
    echo "Ask them to log out / stop their processes, then re-run delete."
    exit 3
  fi

  # Surface the real userdel error rather than hiding it.
  if DEL_ERR="$(userdel "$USERNAME" 2>&1)"; then
    echo "Removed user account."
  else
    echo "ERROR: userdel failed: ${DEL_ERR:-unknown reason}"
    exit 1
  fi
else
  echo "User account not present."
fi

if getent group "$USERNAME" >/dev/null 2>&1; then
  if groupdel "$USERNAME" 2>/dev/null; then
    echo "Removed group."
  else
    echo "Group not removed (may have remaining members)."
  fi
fi

SUDOERS_SUFFIX="${USERNAME//./-}"
SUDOERS_FILE="/etc/sudoers.d/zz-${SUDOERS_SUFFIX}-nopasswd"
if [[ -e "$SUDOERS_FILE" ]]; then
  rm -f "$SUDOERS_FILE"
  echo "Removed sudoers entry $SUDOERS_FILE."
fi

if [[ $SECONDARY -eq 0 ]]; then
  # Primary-only cleanup: the generated key and the shared home.
  if [[ -e "/root/${USERNAME}.pem" ]]; then
    rm -f "/root/${USERNAME}.pem"
    echo "Removed root PEM copy /root/${USERNAME}.pem."
  fi
  if [[ $REMOVE_HOME -eq 1 ]]; then
    if [[ -d "/efs/home/${USERNAME}" ]]; then
      rm -rf "/efs/home/${USERNAME}"
      echo "Removed home /efs/home/${USERNAME}."
    fi
  else
    echo "Left home /efs/home/${USERNAME} in place (no --remove-home)."
  fi
fi

echo "Done for '$USERNAME'."
