#!/usr/bin/env bash
set -euo pipefail
umask 022

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <playit-cli-binary> <playitd-binary> <stage-dir> <systemd-unit-dir>" >&2
  exit 1
fi

CLI_BIN="$1"
DAEMON_BIN="$2"
STAGE_DIR="$3"
SYSTEMD_UNIT_DIR="$4"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="${SCRIPT_DIR}/.."
INSTALL_FOLDER="/opt/playit"

if [[ ! -f "${CLI_BIN}" ]]; then
  echo "playit CLI binary not found: ${CLI_BIN}" >&2
  exit 1
fi

if [[ ! -f "${DAEMON_BIN}" ]]; then
  echo "playit daemon binary not found: ${DAEMON_BIN}" >&2
  exit 1
fi

rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_DIR}${INSTALL_FOLDER}"

cp "${CLI_BIN}" "${STAGE_DIR}${INSTALL_FOLDER}/agent"
cp "${DAEMON_BIN}" "${STAGE_DIR}${INSTALL_FOLDER}/playitd"
chmod 0755 "${STAGE_DIR}${INSTALL_FOLDER}/agent" "${STAGE_DIR}${INSTALL_FOLDER}/playitd"

cat > "${STAGE_DIR}${INSTALL_FOLDER}/playit" <<'EOF'
#!/usr/bin/env bash
/opt/playit/agent "$@"
EOF
chmod 0755 "${STAGE_DIR}${INSTALL_FOLDER}/playit"

mkdir -p "${STAGE_DIR}${SYSTEMD_UNIT_DIR}"
cp "${REPO_DIR}/linux/playit.service" "${STAGE_DIR}${SYSTEMD_UNIT_DIR}/playit.service"

mkdir -p "${STAGE_DIR}/etc/logrotate.d"
cp "${REPO_DIR}/linux/logrotate.conf" "${STAGE_DIR}/etc/logrotate.d/playit"

mkdir -p "${STAGE_DIR}/usr/share/doc/playit"
cp "${REPO_DIR}/LICENSE.txt" "${STAGE_DIR}/usr/share/doc/playit/LICENSE.txt"
