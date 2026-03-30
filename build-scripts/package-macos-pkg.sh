#!/bin/bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <playit-cli-binary> <playitd-binary> <output-pkg>"
  exit 1
fi

cli_path="$1"
daemon_path="$2"
output_pkg="$3"

script_dir=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
repo_root="${script_dir}/.."

export REPO_ROOT="${repo_root}"
version="$(python3 - <<'PY'
import pathlib
import tomllib
import os

with open(pathlib.Path(os.environ["REPO_ROOT"]) / "Cargo.toml", "rb") as handle:
    print(tomllib.load(handle)["workspace"]["package"]["version"])
PY
)"

temp_dir="${script_dir}/temp-build-macos-pkg"
payload_dir="${temp_dir}/root"
scripts_dir="${temp_dir}/scripts"

rm -rf "${temp_dir}"
mkdir -p \
  "${payload_dir}/usr/local/bin" \
  "${payload_dir}/usr/local/libexec/playit" \
  "${scripts_dir}" \
  "$(dirname "${output_pkg}")"

cp "${cli_path}" "${payload_dir}/usr/local/bin/playit"
cp "${daemon_path}" "${payload_dir}/usr/local/bin/playitd"
cp "${script_dir}/macos-launchagent.plist.template" \
  "${payload_dir}/usr/local/libexec/playit/gg.playit.playitd.plist.template"
cp "${script_dir}/macos-uninstall.sh" \
  "${payload_dir}/usr/local/libexec/playit/uninstall-playit-launchagent.sh"
cp "${script_dir}/macos-preinstall.sh" "${scripts_dir}/preinstall"
cp "${script_dir}/macos-postinstall.sh" "${scripts_dir}/postinstall"

chmod 0755 \
  "${payload_dir}/usr/local/bin/playit" \
  "${payload_dir}/usr/local/bin/playitd" \
  "${payload_dir}/usr/local/libexec/playit/uninstall-playit-launchagent.sh" \
  "${scripts_dir}/preinstall" \
  "${scripts_dir}/postinstall"

pkgbuild \
  --root "${payload_dir}" \
  --scripts "${scripts_dir}" \
  --identifier "gg.playit.pkg" \
  --version "${version}" \
  --install-location "/" \
  "${output_pkg}"
