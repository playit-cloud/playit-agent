#!/usr/bin/env bash
set -euo pipefail

START_DIR="$(pwd)"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

if [[ $# -ne 2 && $# -ne 3 ]]; then
  echo "usage: $0 <playit-cli-binary> [playitd-binary] <rpm-arch>" >&2
  exit 1
fi

if ! command -v fpm >/dev/null 2>&1; then
  echo "fpm is required to build RPM packages" >&2
  exit 1
fi

resolve_input_path() {
  if [[ "$1" = /* ]]; then
    printf '%s\n' "$1"
  else
    printf '%s/%s\n' "${START_DIR}" "$1"
  fi
}

CLI_SRC_PATH="$1"

if [[ $# -eq 3 ]]; then
  DAEMON_SRC_PATH="$2"
  RPM_ARCH="$3"
else
  DAEMON_SRC_PATH="$(dirname "${CLI_SRC_PATH}")/playitd"
  RPM_ARCH="$2"
fi

CLI_BIN="$(resolve_input_path "${CLI_SRC_PATH}")"
DAEMON_BIN="$(resolve_input_path "${DAEMON_SRC_PATH}")"
TEMP_DIR_NAME="temp-rpm-${RPM_ARCH}"
ROOT_CARGO_FILE="${SCRIPT_DIR}/../Cargo.toml"
CLI_CARGO_FILE="${SCRIPT_DIR}/../packages/playit-cli/Cargo.toml"
STAGE_DIR="${SCRIPT_DIR}/${TEMP_DIR_NAME}/stage"
SCRIPTLET_DIR="${SCRIPT_DIR}/${TEMP_DIR_NAME}/scriptlets"
OUTPUT_DIR="${SCRIPT_DIR}/../target/rpm"

# shellcheck source=build-scripts/package-metadata.sh
source "${SCRIPT_DIR}/package-metadata.sh"

VERSION="$(package_metadata_workspace_version "${ROOT_CARGO_FILE}")"
MAINTAINER="$(package_metadata_cli_author "${CLI_CARGO_FILE}")"
DESCRIPTION="$(package_metadata_cli_description "${CLI_CARGO_FILE}")"

rm -rf "${SCRIPT_DIR:?}/${TEMP_DIR_NAME}"
mkdir -p "${SCRIPTLET_DIR}" "${OUTPUT_DIR}"

"${SCRIPT_DIR}/package-linux-stage.sh" "${CLI_BIN}" "${DAEMON_BIN}" "${STAGE_DIR}" "/usr/lib/systemd/system"
mkdir -p "${STAGE_DIR}/usr/share/licenses/playit"
cp "${SCRIPT_DIR}/../LICENSE.txt" "${STAGE_DIR}/usr/share/licenses/playit/LICENSE.txt"

cat > "${SCRIPTLET_DIR}/postinst" <<'EOF'
#!/usr/bin/env bash
set -e

mkdir -p /usr/local/bin
ln -sfn /opt/playit/playit /usr/local/bin/playit
getent group playit >/dev/null || groupadd --system playit
install -d -o root -g playit -m 0750 /etc/playit
install -d -o root -g playit -m 0750 /var/log/playit
chown root:playit /etc/playit
chmod 0750 /etc/playit
if [[ -f /etc/playit/playit.toml ]]; then
  chown root:root /etc/playit/playit.toml
  chmod 0600 /etc/playit/playit.toml
fi
chown root:playit /var/log/playit
chmod 0750 /var/log/playit

if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemctl is required to install playit" >&2
  exit 1
fi

LEGACY_UNIT="/etc/systemd/system/playit.service"
if [[ -f "${LEGACY_UNIT}" || -L "${LEGACY_UNIT}" ]]; then
  BACKUP_UNIT="${LEGACY_UNIT}.rpm-bak.$(date -u +%Y%m%d%H%M%S)"
  echo "Moving legacy systemd unit ${LEGACY_UNIT} to ${BACKUP_UNIT} because it shadows the packaged unit"
  mv "${LEGACY_UNIT}" "${BACKUP_UNIT}"
elif [[ -e "${LEGACY_UNIT}" ]]; then
  echo "Cannot install playit: ${LEGACY_UNIT} exists but is not a file or symlink" >&2
  echo "Remove or rename it manually, then reinstall playit." >&2
  exit 1
fi

systemctl daemon-reload
systemctl enable playit
systemctl restart playit || systemctl start playit
EOF

cat > "${SCRIPTLET_DIR}/preun" <<'EOF'
#!/usr/bin/env bash
set -e

if [[ "$1" -eq 0 ]]; then
  if command -v systemctl >/dev/null 2>&1; then
    systemctl stop playit >/dev/null 2>&1 || true
    systemctl disable playit >/dev/null 2>&1 || true
  fi

  if [[ -L "/usr/local/bin/playit" ]]; then
    rm "/usr/local/bin/playit"
  fi
fi
EOF

cat > "${SCRIPTLET_DIR}/postun" <<'EOF'
#!/usr/bin/env bash
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
EOF

chmod 0755 "${SCRIPTLET_DIR}/postinst" "${SCRIPTLET_DIR}/preun" "${SCRIPTLET_DIR}/postun"

rm -f "${OUTPUT_DIR}/playit_${RPM_ARCH}.rpm"
fpm \
  -s dir \
  -t rpm \
  -C "${STAGE_DIR}" \
  -n playit \
  -v "${VERSION}" \
  --iteration 1 \
  --architecture "${RPM_ARCH}" \
  --description "${DESCRIPTION}" \
  --maintainer "${MAINTAINER}" \
  --url "https://github.com/playit-cloud/playit-agent" \
  --license "BSD-2-Clause" \
  --depends logrotate \
  --rpm-user root \
  --rpm-group root \
  --after-install "${SCRIPTLET_DIR}/postinst" \
  --before-remove "${SCRIPTLET_DIR}/preun" \
  --after-remove "${SCRIPTLET_DIR}/postun" \
  --package "${OUTPUT_DIR}/playit_${RPM_ARCH}.rpm" \
  .
