#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_GROUP=playit
PLAYIT_HOME=/nonexistent
PLAYIT_MANAGER_FILE=/opt/playit/share/init/selected-manager
SYSUSERS_CONFIG=/usr/lib/sysusers.d/playit.conf
SYSTEMD_UNIT=/usr/lib/systemd/system/playit.service

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

  shell="$(nologin_shell)"
  if have_command useradd; then
    useradd --system --gid "$PLAYIT_GROUP" --home-dir "$PLAYIT_HOME" --no-create-home --shell "$shell" "$PLAYIT_USER"
  elif have_command adduser; then
    adduser -S -D -H -h "$PLAYIT_HOME" -s "$shell" -G "$PLAYIT_GROUP" "$PLAYIT_USER"
  else
    echo "Cannot create ${PLAYIT_USER} user: useradd/adduser not found" >&2
    exit 1
  fi
}

provision_user() {
  if have_command systemd-sysusers; then
    systemd-sysusers "$SYSUSERS_CONFIG"
  else
    ensure_group
    ensure_user
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

handle_legacy_systemd_unit() {
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
}

remove_legacy_unit_path() {
  legacy_unit="$1"

  if [ ! -e "$legacy_unit" ] && [ ! -L "$legacy_unit" ]; then
    return 0
  fi

  if [ "$(readlink -f "$legacy_unit" 2>/dev/null || printf '%s\n' "$legacy_unit")" = "$(readlink -f "$SYSTEMD_UNIT" 2>/dev/null || printf '%s\n' "$SYSTEMD_UNIT")" ]; then
    return 0
  fi

  rm -f "$legacy_unit"
}

provision_user

mkdir -p /usr/bin /etc/playit /opt/playit/share/init
ln -sfn /opt/playit/playit /usr/bin/playit
ln -sfn /opt/playit/playitd /usr/bin/playitd

chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit
chmod 0750 /etc/playit

if [ -f /var/log/playit/playit.log ]; then
  chmod 0640 /var/log/playit/playit.log
fi

printf '%s\n' systemd > "$PLAYIT_MANAGER_FILE"
chmod 0644 "$PLAYIT_MANAGER_FILE"

if [ -f /etc/playit/playit.toml ]; then
  chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit/playit.toml
  chmod 0600 /etc/playit/playit.toml
fi

handle_legacy_systemd_unit
remove_legacy_unit_path /lib/systemd/system/playit.service

if have_command systemctl; then
  systemctl daemon-reload || true
  systemctl enable playit || true

  if is_fresh_install "$@"; then
    systemctl start playit || true
  elif systemctl is-active --quiet playit; then
    systemctl restart playit || true
  fi
fi
