#!/bin/bash
# Removes playit from this Mac for all users. Secrets in each user's
# ~/Library/Application Support/playit_gg are kept so a reinstall reconnects the same agent.
set -u

if [[ "$(id -u)" -ne 0 ]]; then
  exec sudo "$0" "$@"
fi

APP="/Applications/playit.app"

for home in /Users/*; do
  user="$(basename "${home}")"
  uid="$(id -u "${user}" 2>/dev/null)" || continue
  for label in gg.playit.playitd gg.playit.tray; do
    launchctl bootout "gui/${uid}/${label}" >/dev/null 2>&1 || true
    rm -f "${home}/Library/LaunchAgents/${label}.plist"
  done
done

pkill -x playitd-tray >/dev/null 2>&1 || true

if [[ "$(readlink /usr/local/bin/playit 2>/dev/null)" == "${APP}/"* ]]; then
  rm -f /usr/local/bin/playit
fi
rm -rf "${APP}"
pkgutil --forget gg.playit.pkg >/dev/null 2>&1 || true

echo "playit was removed. Agent secrets remain in ~/Library/Application Support/playit_gg."
