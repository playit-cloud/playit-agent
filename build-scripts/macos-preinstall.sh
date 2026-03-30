#!/bin/bash
set -euo pipefail

console_user="$(stat -f%Su /dev/console 2>/dev/null || true)"

if [[ -z "${console_user}" || "${console_user}" == "root" || "${console_user}" == "loginwindow" ]]; then
  exit 0
fi

user_uid="$(id -u "${console_user}")"
user_home="$(dscl . -read "/Users/${console_user}" NFSHomeDirectory | awk '{print $2}')"
plist_path="${user_home}/Library/LaunchAgents/gg.playit.playitd.plist"

if [[ -f "${plist_path}" ]]; then
  launchctl bootout "gui/${user_uid}" "${plist_path}" >/dev/null 2>&1 || true
fi
