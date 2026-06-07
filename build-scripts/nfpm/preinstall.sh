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

ensure_sysusers_user() {
  if user_exists && group_exists; then
    return 0
  fi

  if ! have_command systemd-sysusers; then
    echo "Cannot provision ${PLAYIT_USER} user with systemd-sysusers: command not found" >&2
    exit 1
  fi

  systemd-sysusers --inline "u ${PLAYIT_USER} - \"playit service user\" ${PLAYIT_HOME} $(nologin_shell)"
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

case "$(detect_init_manager)" in
  systemd)
    ensure_sysusers_user
    ;;
  *)
    ensure_group
    ensure_user
    ;;
esac
