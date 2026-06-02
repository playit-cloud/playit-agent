#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <version> <release-asset-dir>" >&2
  exit 1
fi

VERSION="$1"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_DIR="${SCRIPT_DIR}/.."
ROOT_CARGO_FILE="${REPO_DIR}/Cargo.toml"
CLI_CARGO_FILE="${REPO_DIR}/packages/playit-cli/Cargo.toml"
AUR_DIR="${REPO_DIR}/target/aur/playit-bin"

if [[ "$2" = /* ]]; then
  RELEASE_ASSET_DIR="$2"
else
  RELEASE_ASSET_DIR="$(pwd)/$2"
fi

# shellcheck source=build-scripts/package-metadata.sh
source "${SCRIPT_DIR}/package-metadata.sh"

DESCRIPTION="$(package_metadata_cli_description "${CLI_CARGO_FILE}")"
MAINTAINER="$(package_metadata_cli_author "${CLI_CARGO_FILE}")"
WORKSPACE_VERSION="$(package_metadata_workspace_version "${ROOT_CARGO_FILE}")"

if [[ "${VERSION}" != "${WORKSPACE_VERSION}" ]]; then
  echo "warning: requested AUR version ${VERSION} does not match workspace version ${WORKSPACE_VERSION}" >&2
fi

sha256_file() {
  local file="$1"
  if [[ ! -f "${file}" ]]; then
    echo "missing release asset: ${file}" >&2
    exit 1
  fi
  sha256sum "${file}" | awk '{print $1}'
}

checksum_pair() {
  local arch="$1"
  printf "    '%s'\n    '%s'\n" \
    "$(sha256_file "${RELEASE_ASSET_DIR}/playit-cli-linux-${arch}")" \
    "$(sha256_file "${RELEASE_ASSET_DIR}/playit-linux-${arch}")"
}

rm -rf "${AUR_DIR}"
mkdir -p "${AUR_DIR}"
cp "${REPO_DIR}/linux/playit.service" "${AUR_DIR}/playit.service"
cp "${REPO_DIR}/linux/logrotate.conf" "${AUR_DIR}/logrotate.conf"
cp "${REPO_DIR}/LICENSE.txt" "${AUR_DIR}/LICENSE.txt"

SERVICE_SHA="$(sha256_file "${AUR_DIR}/playit.service")"
LOGROTATE_SHA="$(sha256_file "${AUR_DIR}/logrotate.conf")"
LICENSE_SHA="$(sha256_file "${AUR_DIR}/LICENSE.txt")"
AMD64_SUMS="$(checksum_pair amd64)"
AARCH64_SUMS="$(checksum_pair aarch64)"
ARMV7_SUMS="$(checksum_pair armv7)"
I686_SUMS="$(checksum_pair i686)"

cat > "${AUR_DIR}/PKGBUILD" <<EOF
# Maintainer: ${MAINTAINER}
pkgname=playit-bin
pkgver=${VERSION}
pkgrel=1
pkgdesc='${DESCRIPTION}'
arch=('x86_64' 'aarch64' 'armv7h' 'i686')
url='https://github.com/playit-cloud/playit-agent'
license=('BSD-2-Clause')
depends=('logrotate')
provides=('playit')
conflicts=('playit')
install="\${pkgname}.install"
source=('playit.service' 'logrotate.conf' 'LICENSE.txt')
source_x86_64=(
    "playit-cli-linux-amd64-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-cli-linux-amd64"
    "playit-linux-amd64-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-linux-amd64"
)
source_aarch64=(
    "playit-cli-linux-aarch64-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-cli-linux-aarch64"
    "playit-linux-aarch64-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-linux-aarch64"
)
source_armv7h=(
    "playit-cli-linux-armv7-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-cli-linux-armv7"
    "playit-linux-armv7-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-linux-armv7"
)
source_i686=(
    "playit-cli-linux-i686-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-cli-linux-i686"
    "playit-linux-i686-\${pkgver}::\${url}/releases/download/v\${pkgver}/playit-linux-i686"
)
sha256sums=(
    '${SERVICE_SHA}'
    '${LOGROTATE_SHA}'
    '${LICENSE_SHA}'
)
sha256sums_x86_64=(
${AMD64_SUMS}
)
sha256sums_aarch64=(
${AARCH64_SUMS}
)
sha256sums_armv7h=(
${ARMV7_SUMS}
)
sha256sums_i686=(
${I686_SUMS}
)

package() {
    local asset_arch
    case "\${CARCH}" in
        x86_64) asset_arch='amd64' ;;
        aarch64) asset_arch='aarch64' ;;
        armv7h) asset_arch='armv7' ;;
        i686) asset_arch='i686' ;;
        *) echo "unsupported architecture: \${CARCH}" >&2; return 1 ;;
    esac

    install -Dm755 "\${srcdir}/playit-cli-linux-\${asset_arch}-\${pkgver}" "\${pkgdir}/opt/playit/agent"
    install -Dm755 "\${srcdir}/playit-linux-\${asset_arch}-\${pkgver}" "\${pkgdir}/opt/playit/playitd"
    install -Dm644 "\${srcdir}/playit.service" "\${pkgdir}/usr/lib/systemd/system/playit.service"
    install -Dm644 "\${srcdir}/logrotate.conf" "\${pkgdir}/etc/logrotate.d/playit"
    install -Dm644 "\${srcdir}/LICENSE.txt" "\${pkgdir}/usr/share/licenses/\${pkgname}/LICENSE"

    install -Dm755 /dev/stdin "\${pkgdir}/opt/playit/playit" <<'WRAPPER'
#!/usr/bin/env bash
/opt/playit/agent "\$@"
WRAPPER
}
EOF

cat > "${AUR_DIR}/playit-bin.install" <<'EOF'
setup_playit_service() {
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
    return 1
  fi

  LEGACY_UNIT="/etc/systemd/system/playit.service"
  if [[ -f "${LEGACY_UNIT}" || -L "${LEGACY_UNIT}" ]]; then
    BACKUP_UNIT="${LEGACY_UNIT}.pacman-bak.$(date -u +%Y%m%d%H%M%S)"
    echo "Moving legacy systemd unit ${LEGACY_UNIT} to ${BACKUP_UNIT} because it shadows the packaged unit"
    mv "${LEGACY_UNIT}" "${BACKUP_UNIT}"
  elif [[ -e "${LEGACY_UNIT}" ]]; then
    echo "Cannot install playit: ${LEGACY_UNIT} exists but is not a file or symlink" >&2
    echo "Remove or rename it manually, then reinstall playit." >&2
    return 1
  fi

  systemctl daemon-reload
  systemctl enable playit
  systemctl restart playit || systemctl start playit
}

post_install() {
  setup_playit_service
}

post_upgrade() {
  setup_playit_service
}

pre_remove() {
  if command -v systemctl >/dev/null 2>&1; then
    systemctl stop playit >/dev/null 2>&1 || true
    systemctl disable playit >/dev/null 2>&1 || true
  fi

  if [[ -L "/usr/local/bin/playit" ]]; then
    rm "/usr/local/bin/playit"
  fi
}

post_remove() {
  if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
  fi
}
EOF

generate_srcinfo_fallback() {
  cat > "${AUR_DIR}/.SRCINFO" <<EOF
pkgbase = playit-bin
	pkgdesc = ${DESCRIPTION}
	pkgver = ${VERSION}
	pkgrel = 1
	url = https://github.com/playit-cloud/playit-agent
	arch = x86_64
	arch = aarch64
	arch = armv7h
	arch = i686
	license = BSD-2-Clause
	depends = logrotate
	provides = playit
	conflicts = playit
	source = playit.service
	source = logrotate.conf
	source = LICENSE.txt
	source_x86_64 = playit-cli-linux-amd64-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-cli-linux-amd64
	source_x86_64 = playit-linux-amd64-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-linux-amd64
	source_aarch64 = playit-cli-linux-aarch64-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-cli-linux-aarch64
	source_aarch64 = playit-linux-aarch64-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-linux-aarch64
	source_armv7h = playit-cli-linux-armv7-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-cli-linux-armv7
	source_armv7h = playit-linux-armv7-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-linux-armv7
	source_i686 = playit-cli-linux-i686-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-cli-linux-i686
	source_i686 = playit-linux-i686-${VERSION}::https://github.com/playit-cloud/playit-agent/releases/download/v${VERSION}/playit-linux-i686
	sha256sums = ${SERVICE_SHA}
	sha256sums = ${LOGROTATE_SHA}
	sha256sums = ${LICENSE_SHA}
	sha256sums_x86_64 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-cli-linux-amd64")
	sha256sums_x86_64 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-linux-amd64")
	sha256sums_aarch64 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-cli-linux-aarch64")
	sha256sums_aarch64 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-linux-aarch64")
	sha256sums_armv7h = $(sha256_file "${RELEASE_ASSET_DIR}/playit-cli-linux-armv7")
	sha256sums_armv7h = $(sha256_file "${RELEASE_ASSET_DIR}/playit-linux-armv7")
	sha256sums_i686 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-cli-linux-i686")
	sha256sums_i686 = $(sha256_file "${RELEASE_ASSET_DIR}/playit-linux-i686")

pkgname = playit-bin
	install = playit-bin.install
EOF
}

(
  cd "${AUR_DIR}"
  if command -v makepkg >/dev/null 2>&1; then
    makepkg --printsrcinfo > .SRCINFO
  else
    echo "makepkg not found; generating .SRCINFO without makepkg" >&2
    generate_srcinfo_fallback
  fi
)
