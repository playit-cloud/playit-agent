#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_GROUP=playit
PLAYIT_HOME=/nonexistent
PLAYIT_MANAGER_FILE=/opt/playit/share/init/selected-manager

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

# Lets whoever installed playit with sudo or pkexec use it right away. playitd reads the group
# from the account database on each connection, so this works without logging out.
add_installing_user_to_group() {
  installing_user="${SUDO_USER:-}"
  if [ -z "$installing_user" ] && [ -n "${PKEXEC_UID:-}" ]; then
    installing_user="$(id -nu "$PKEXEC_UID" 2>/dev/null || true)"
  fi

  case "$installing_user" in
    ""|root|"$PLAYIT_USER")
      return 0
      ;;
  esac

  if ! id -u "$installing_user" >/dev/null 2>&1; then
    return 0
  fi
  if id -nG "$installing_user" 2>/dev/null | tr ' ' '\n' | grep -qx "$PLAYIT_GROUP"; then
    return 0
  fi

  if have_command usermod; then
    usermod -a -G "$PLAYIT_GROUP" "$installing_user" || true
  elif have_command addgroup; then
    addgroup "$installing_user" "$PLAYIT_GROUP" || true
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

fresh_install=false
if is_fresh_install "$@" || [ ! -f "$PLAYIT_MANAGER_FILE" ]; then
  fresh_install=true
fi

ensure_group
ensure_user
if [ "$fresh_install" = true ]; then
  add_installing_user_to_group
fi

mkdir -p /usr/bin /etc/playit /var/log/playit /opt/playit/share/init
ln -sfn /opt/playit/playit /usr/bin/playit
ln -sfn /opt/playit/playitd /usr/bin/playitd

chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit
chown -R "$PLAYIT_USER:$PLAYIT_GROUP" /var/log/playit
chmod 0750 /etc/playit /var/log/playit

if [ -f /var/log/playit/playit.log ]; then
  chmod 0640 /var/log/playit/playit.log
fi

printf '%s\n' openrc > "$PLAYIT_MANAGER_FILE"
chmod 0644 "$PLAYIT_MANAGER_FILE"

if [ -f /etc/playit/playit.toml ]; then
  chown "$PLAYIT_USER:$PLAYIT_GROUP" /etc/playit/playit.toml
  chmod 0600 /etc/playit/playit.toml
fi

if [ "$fresh_install" = true ]; then
  rc-update add playit default || true
  rc-service playit start || true
elif rc-service playit status >/dev/null 2>&1; then
  rc-service playit restart || true
fi
