#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_GROUP=playit
PLAYIT_HOME=/var/lib/playit

have_command() {
  command -v "$1" >/dev/null 2>&1
}

nologin_shell() {
  if [ -x /usr/sbin/nologin ]; then
    printf '%s\n' /usr/sbin/nologin
  elif [ -x /sbin/nologin ]; then
    printf '%s\n' /sbin/nologin
  else
    printf '%s\n' /bin/false
  fi
}

group_exists() {
  if have_command getent; then
    getent group "$PLAYIT_GROUP" >/dev/null 2>&1
  else
    grep -q "^${PLAYIT_GROUP}:" /etc/group 2>/dev/null
  fi
}

user_exists() {
  if have_command id; then
    id -u "$PLAYIT_USER" >/dev/null 2>&1
  else
    grep -q "^${PLAYIT_USER}:" /etc/passwd 2>/dev/null
  fi
}

ensure_group() {
  if group_exists; then
    return 0
  fi

  if have_command groupadd; then
    groupadd --system "$PLAYIT_GROUP"
  elif have_command addgroup; then
    addgroup -S "$PLAYIT_GROUP"
  else
    echo "Cannot create ${PLAYIT_GROUP} group: groupadd/addgroup not found" >&2
    exit 1
  fi
}

ensure_user() {
  if user_exists; then
    return 0
  fi

  if have_command useradd; then
    useradd --system --gid "$PLAYIT_GROUP" --home-dir "$PLAYIT_HOME" --no-create-home --shell "$(nologin_shell)" "$PLAYIT_USER"
  elif have_command adduser; then
    adduser -S -D -H -h "$PLAYIT_HOME" -s "$(nologin_shell)" -G "$PLAYIT_GROUP" "$PLAYIT_USER"
  else
    echo "Cannot create ${PLAYIT_USER} user: useradd/adduser not found" >&2
    exit 1
  fi
}

is_fresh_install() {
  case "${1:-}" in
    ""|0|1|install)
      return 0
      ;;
    configure)
      [ -z "${2:-}" ]
      return
      ;;
    *)
      return 1
      ;;
  esac
}

ensure_group
ensure_user

mkdir -p /usr/local/bin /etc/playit /var/log/playit
ln -sfn /opt/playit/playit /usr/local/bin/playit

chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit /var/log/playit
chmod 0750 /etc/playit /var/log/playit

if [ -f /etc/playit/playit.toml ]; then
  chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit/playit.toml
  chmod 0600 /etc/playit/playit.toml
fi

LEGACY_UNIT=/etc/systemd/system/playit.service
if [ -f "$LEGACY_UNIT" ] || [ -L "$LEGACY_UNIT" ]; then
  BACKUP_UNIT="${LEGACY_UNIT}.dpkg-bak.$(date -u +%Y%m%d%H%M%S)"
  echo "Moving legacy systemd unit ${LEGACY_UNIT} to ${BACKUP_UNIT} because it shadows the packaged unit"
  mv "$LEGACY_UNIT" "$BACKUP_UNIT"
elif [ -e "$LEGACY_UNIT" ]; then
  echo "Cannot install playit: ${LEGACY_UNIT} exists but is not a file or symlink" >&2
  echo "Remove or rename it manually, then reinstall playit." >&2
  exit 1
fi

if ! have_command systemctl; then
  echo "systemctl is unavailable; installed playit without enabling or starting the service"
  exit 0
fi

systemctl daemon-reload || true
systemctl enable playit || true

if is_fresh_install "$@"; then
  systemctl start playit || true
elif systemctl is-active --quiet playit; then
  systemctl restart playit || true
fi
