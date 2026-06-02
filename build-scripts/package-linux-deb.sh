#!/usr/bin/env bash
set -euo pipefail

START_DIR="$(pwd)"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

if [[ $# -ne 2 && $# -ne 3 ]]; then
  echo "usage: $0 <playit-cli-binary> [playitd-binary] <deb-arch>" >&2
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
  DEB_ARCH="$3"
else
  DAEMON_SRC_PATH="$(dirname "${CLI_SRC_PATH}")/playitd"
  DEB_ARCH="$2"
fi

CLI_BIN="$(resolve_input_path "${CLI_SRC_PATH}")"
DAEMON_BIN="$(resolve_input_path "${DAEMON_SRC_PATH}")"
TEMP_DIR_NAME="temp-build-${DEB_ARCH}"
DEB_PACKAGE="playit_${DEB_ARCH}"
ROOT_CARGO_FILE="${SCRIPT_DIR}/../Cargo.toml"
CLI_CARGO_FILE="${SCRIPT_DIR}/../packages/playit-cli/Cargo.toml"
INSTALL_FOLDER="/opt/playit"

# shellcheck source=build-scripts/package-metadata.sh
source "${SCRIPT_DIR}/package-metadata.sh"

VERSION="$(package_metadata_workspace_version "${ROOT_CARGO_FILE}")"
MAINTAINER="$(package_metadata_cli_author "${CLI_CARGO_FILE}")"
DESCRIPTION="$(package_metadata_cli_description "${CLI_CARGO_FILE}")"

echo "PREPARE TEMP BUILD FOLDER"
cd "${SCRIPT_DIR}"
rm -rf "${TEMP_DIR_NAME}"
mkdir -p "${TEMP_DIR_NAME}"

WK_DIR="${SCRIPT_DIR}/${TEMP_DIR_NAME}/${DEB_PACKAGE}"

echo "PREPARE PACKAGE FILES"
"${SCRIPT_DIR}/package-linux-stage.sh" "${CLI_BIN}" "${DAEMON_BIN}" "${WK_DIR}" "/lib/systemd/system"

echo "BUILD DEB CONFIG FILES"
mkdir -p "${WK_DIR}/DEBIAN"
cat > "${WK_DIR}/DEBIAN/control" <<EOF
Package: playit
Version: ${VERSION}
Architecture: ${DEB_ARCH}
Maintainer: ${MAINTAINER}
Description: ${DESCRIPTION}
Depends: logrotate
EOF

cat > "${WK_DIR}/DEBIAN/postinst" <<EOF
#!/usr/bin/env bash
set -e

mkdir -p /usr/local/bin
ln -sfn ${INSTALL_FOLDER}/playit /usr/local/bin/playit
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
if [[ -f "\${LEGACY_UNIT}" || -L "\${LEGACY_UNIT}" ]]; then
  BACKUP_UNIT="\${LEGACY_UNIT}.dpkg-bak.\$(date -u +%Y%m%d%H%M%S)"
  echo "Moving legacy systemd unit \${LEGACY_UNIT} to \${BACKUP_UNIT} because it shadows the packaged unit"
  mv "\${LEGACY_UNIT}" "\${BACKUP_UNIT}"
elif [[ -e "\${LEGACY_UNIT}" ]]; then
  echo "Cannot install playit: \${LEGACY_UNIT} exists but is not a file or symlink" >&2
  echo "Remove or rename it manually, then reinstall playit." >&2
  exit 1
fi

systemctl daemon-reload
systemctl enable playit
systemctl restart playit || systemctl start playit
EOF
chmod 0555 "${WK_DIR}/DEBIAN/postinst"

cat > "${WK_DIR}/DEBIAN/prerm" <<'EOF'
#!/usr/bin/env bash
if [[ -L "/usr/local/bin/playit" ]]; then
  rm "/usr/local/bin/playit"
fi
EOF
chmod 0555 "${WK_DIR}/DEBIAN/prerm"

cd "${SCRIPT_DIR}/${TEMP_DIR_NAME}"
dpkg-deb --build -Zgzip --root-owner-group "${DEB_PACKAGE}"

mkdir -p "${SCRIPT_DIR}/../target/deb"
cp "${DEB_PACKAGE}.deb" "${SCRIPT_DIR}/../target/deb"
