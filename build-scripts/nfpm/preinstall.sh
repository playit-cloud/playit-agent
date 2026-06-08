#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_GROUP=playit
PLAYIT_HOME=/nonexistent
SYSUSERS_CONFIG=/usr/lib/sysusers.d/playit.conf
SYSUSERS_SHELL=/usr/sbin/nologin

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

if have_command systemd-sysusers && [ -f "$SYSUSERS_CONFIG" ]; then
  systemd-sysusers "$SYSUSERS_CONFIG"
elif have_command systemd-sysusers; then
  systemd-sysusers --inline "u ${PLAYIT_USER} - \"playit service user\" ${PLAYIT_HOME} ${SYSUSERS_SHELL}"
else
  ensure_group
  ensure_user
fi
