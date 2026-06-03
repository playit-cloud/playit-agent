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

if have_command systemctl; then
  systemctl stop playit || true
  systemctl disable playit || true
fi

if [ -L /usr/local/bin/playit ]; then
  rm -f /usr/local/bin/playit
fi

if [ -L /usr/local/bin/playitd ]; then
  rm -f /usr/local/bin/playitd
fi
