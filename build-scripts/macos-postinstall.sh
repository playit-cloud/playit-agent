#!/bin/bash
set -euo pipefail

console_user="$(stat -f%Su /dev/console 2>/dev/null || true)"

if [[ -z "${console_user}" || "${console_user}" == "root" || "${console_user}" == "loginwindow" ]]; then
  echo "No active macOS user session detected; playit binaries were installed, but the LaunchAgent was not bootstrapped."
  exit 0
fi

user_uid="$(id -u "${console_user}")"
user_group="$(id -gn "${console_user}")"
user_home="$(dscl . -read "/Users/${console_user}" NFSHomeDirectory | awk '{print $2}')"

launch_agents_dir="${user_home}/Library/LaunchAgents"
playit_data_dir="${user_home}/Library/Application Support/playit_gg"
playit_log_dir="${user_home}/Library/Logs/playit"

plist_template="/usr/local/libexec/playit/gg.playit.playitd.plist.template"
plist_path="${launch_agents_dir}/gg.playit.playitd.plist"
playitd_path="/usr/local/bin/playitd"
socket_path="${playit_data_dir}/playitd.sock"
secret_path="${playit_data_dir}/playit.toml"
log_path="${playit_log_dir}/playitd.log"
stdout_path="${playit_log_dir}/playitd.stdout.log"
stderr_path="${playit_log_dir}/playitd.stderr.log"

mkdir -p "${launch_agents_dir}" "${playit_data_dir}" "${playit_log_dir}"

sed \
  -e "s|__PLAYITD_PATH__|${playitd_path}|g" \
  -e "s|__PLAYIT_SOCKET_PATH__|${socket_path}|g" \
  -e "s|__PLAYIT_SECRET_PATH__|${secret_path}|g" \
  -e "s|__PLAYIT_LOG_PATH__|${log_path}|g" \
  -e "s|__PLAYIT_WORK_DIR__|${playit_data_dir}|g" \
  -e "s|__PLAYIT_STDOUT_PATH__|${stdout_path}|g" \
  -e "s|__PLAYIT_STDERR_PATH__|${stderr_path}|g" \
  "${plist_template}" > "${plist_path}"

chown -R "${console_user}:${user_group}" \
  "${launch_agents_dir}" \
  "${playit_data_dir}" \
  "${playit_log_dir}"

launchctl bootout "gui/${user_uid}" "${plist_path}" >/dev/null 2>&1 || true
launchctl bootstrap "gui/${user_uid}" "${plist_path}"
launchctl enable "gui/${user_uid}/gg.playit.playitd" >/dev/null 2>&1 || true
launchctl kickstart -k "gui/${user_uid}/gg.playit.playitd"
