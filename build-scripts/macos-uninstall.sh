#!/bin/bash
set -euo pipefail

target_user="${1:-$(stat -f%Su /dev/console 2>/dev/null || true)}"

if [[ -z "${target_user}" || "${target_user}" == "root" || "${target_user}" == "loginwindow" ]]; then
  echo "No target user detected. Pass the macOS username as the first argument."
  exit 1
fi

target_uid="$(id -u "${target_user}")"
target_home="$(dscl . -read "/Users/${target_user}" NFSHomeDirectory | awk '{print $2}')"

plist_path="${target_home}/Library/LaunchAgents/gg.playit.playitd.plist"
playit_data_dir="${target_home}/Library/Application Support/playit_gg"
playit_log_dir="${target_home}/Library/Logs/playit"

launchctl bootout "gui/${target_uid}" "${plist_path}" >/dev/null 2>&1 || true

rm -f "${plist_path}"
rm -f /usr/local/bin/playit /usr/local/bin/playitd
rm -f /usr/local/libexec/playit/gg.playit.playitd.plist.template
rm -f /usr/local/libexec/playit/uninstall-playit-launchagent.sh

if [[ -d /usr/local/libexec/playit ]] && [[ -z "$(ls -A /usr/local/libexec/playit)" ]]; then
  rmdir /usr/local/libexec/playit
fi

rm -rf "${playit_data_dir}" "${playit_log_dir}"
