#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_GROUP=playit
PLAYIT_HOME=/nonexistent
PLAYIT_MANAGER_FILE=/opt/playit/share/init/selected-manager
PACKAGED_SYSUSERS_CONFIG=/opt/playit/share/init/systemd/playit.sysusers
SYSUSERS_CONFIG=/usr/lib/sysusers.d/playit.conf
SYSTEMD_TEMPLATE=/opt/playit/share/init/systemd/playit.service
OPENRC_TEMPLATE=/opt/playit/share/init/openrc/playit
OPENRC_SERVICE=/etc/init.d/playit

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

ensure_sysusers_user() {
  if user_exists && group_exists; then
    return 0
  fi

  if ! have_command systemd-sysusers; then
    echo "Cannot provision ${PLAYIT_USER} user with systemd-sysusers: command not found" >&2
    exit 1
  fi

  if [ ! -f "$SYSUSERS_CONFIG" ]; then
    mkdir -p "$(dirname "$SYSUSERS_CONFIG")"
    install -m 0644 "$PACKAGED_SYSUSERS_CONFIG" "$SYSUSERS_CONFIG"
  fi

  systemd-sysusers "$SYSUSERS_CONFIG"
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

systemd_is_active() {
  have_command systemctl && [ -d /run/systemd/system ]
}

systemd_is_installed() {
  have_command systemctl
}

openrc_is_active() {
  have_command rc-service &&
    have_command rc-update &&
    [ -x /sbin/openrc-run ] &&
    { [ -e /run/openrc/softlevel ] || [ -d /run/openrc ]; }
}

openrc_is_installed() {
  have_command rc-service &&
    have_command rc-update &&
    [ -x /sbin/openrc-run ]
}

detect_init_manager() {
  systemd_active=0
  openrc_active=0
  systemd_installed=0
  openrc_installed=0

  systemd_is_active && systemd_active=1
  openrc_is_active && openrc_active=1
  systemd_is_installed && systemd_installed=1
  openrc_is_installed && openrc_installed=1

  if [ "$systemd_active" -eq 1 ] && [ "$openrc_active" -eq 0 ]; then
    echo systemd
  elif [ "$openrc_active" -eq 1 ] && [ "$systemd_active" -eq 0 ]; then
    echo openrc
  elif [ "$systemd_active" -eq 1 ] && [ "$openrc_active" -eq 1 ]; then
    echo systemd
  elif [ "$systemd_installed" -eq 1 ] && [ "$openrc_installed" -eq 0 ]; then
    echo systemd
  elif [ "$openrc_installed" -eq 1 ] && [ "$systemd_installed" -eq 0 ]; then
    echo openrc
  else
    echo none
  fi
}

systemd_unit_path() {
  if [ -d /lib/systemd/system ] || [ -L /lib/systemd/system ]; then
    echo /lib/systemd/system/playit.service
  else
    echo /usr/lib/systemd/system/playit.service
  fi
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

manager="$(detect_init_manager)"

case "$manager" in
  systemd)
    ensure_sysusers_user
    ;;
  *)
    ensure_group
    ensure_user
    ;;
esac

mkdir -p /usr/local/bin /etc/playit /var/log/playit
ln -sfn /opt/playit/playit /usr/local/bin/playit
ln -sfn /opt/playit/playitd /usr/local/bin/playitd

chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit
chown -R "$PLAYIT_USER:$PLAYIT_GROUP" /var/log/playit
chmod 0750 /etc/playit /var/log/playit

if [ -f /var/log/playit/playit.log ]; then
  chmod 0640 /var/log/playit/playit.log
fi

mkdir -p /opt/playit/share/init
printf '%s\n' "$manager" > "$PLAYIT_MANAGER_FILE"
chmod 0644 "$PLAYIT_MANAGER_FILE"

if [ "$manager" != "none" ] && [ -f /etc/playit/playit.toml ]; then
  chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit/playit.toml
  chmod 0600 /etc/playit/playit.toml
fi

case "$manager" in
  systemd)
    handle_legacy_systemd_unit
    unit="$(systemd_unit_path)"
    mkdir -p "$(dirname "$unit")"
    install -m 0644 "$SYSTEMD_TEMPLATE" "$unit"
    rm -f "$OPENRC_SERVICE"

    systemctl daemon-reload || true
    systemctl enable playit || true

    if is_fresh_install "$@"; then
      systemctl start playit || true
    elif systemctl is-active --quiet playit; then
      systemctl restart playit || true
    fi
    ;;
  openrc)
    mkdir -p "$(dirname "$OPENRC_SERVICE")"
    install -m 0755 "$OPENRC_TEMPLATE" "$OPENRC_SERVICE"
    rm -f /lib/systemd/system/playit.service
    rm -f /usr/lib/systemd/system/playit.service

    rc-update add playit default || true

    if is_fresh_install "$@"; then
      rc-service playit start || true
    elif rc-service playit status >/dev/null 2>&1; then
      rc-service playit restart || true
    fi
    ;;
  *)
    rm -f /lib/systemd/system/playit.service
    rm -f /usr/lib/systemd/system/playit.service
    rm -f "$OPENRC_SERVICE"
    echo "No supported init manager detected; installed playit binaries without a service file."
    ;;
esac
