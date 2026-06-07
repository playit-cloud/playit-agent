#!/bin/sh
set -eu

have_command() {
  command -v "$1" >/dev/null 2>&1
}

is_upgrade() {
  case "${1:-}" in
    upgrade|upgrading|1)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

if is_upgrade "${1:-}"; then
  exit 0
fi

if have_command rc-service; then
  rc-service playit stop || true
fi

if have_command rc-update; then
  rc-update del playit default || true
fi

rm -f /etc/init.d/playit
rm -f /opt/playit/share/init/selected-manager

if [ -L /usr/bin/playit ]; then
  rm -f /usr/bin/playit
fi

if [ -L /usr/bin/playitd ]; then
  rm -f /usr/bin/playitd
fi

if [ -L /usr/local/bin/playit ]; then
  rm -f /usr/local/bin/playit
fi

if [ -L /usr/local/bin/playitd ]; then
  rm -f /usr/local/bin/playitd
fi
