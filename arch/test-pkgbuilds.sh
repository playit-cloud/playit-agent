#!/usr/bin/env bash
set -euo pipefail

IMAGE="${PLAYIT_ARCH_TEST_IMAGE:-docker.io/library/alpine:latest}"
SCRIPT_PATH="${BASH_SOURCE[0]}"
REPO_DIR="$(cd -- "$(dirname -- "${SCRIPT_PATH}")/.." && pwd)"

usage() {
  cat >&2 <<EOF
usage: $0 [--inside-container]

Runs Arch PKGBUILD smoke tests in an Alpine container using podman.

Environment:
  PLAYIT_ARCH_TEST_IMAGE  Container image to use (default: ${IMAGE})
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" != "--inside-container" ]]; then
  if ! command -v podman >/dev/null 2>&1; then
    echo "podman is required to run Arch PKGBUILD tests" >&2
    exit 1
  fi

  exec podman run --rm \
    --volume "${REPO_DIR}:/work:Z" \
    --workdir /work \
    "${IMAGE}" \
    /bin/sh -ceu '
      apk add --no-cache bash build-base ca-certificates cargo coreutils curl file findutils git rust
      exec bash arch/test-pkgbuilds.sh --inside-container
    '
fi

require_file() {
  if [[ ! -f "$1" ]]; then
    echo "missing required file: $1" >&2
    exit 1
  fi
}

source_url() {
  local source_entry="$1"

  if [[ "${source_entry}" == *::* ]]; then
    printf '%s\n' "${source_entry#*::}"
  else
    printf '%s\n' "${source_entry}"
  fi
}

source_name() {
  local source_entry="$1"
  local source_path

  if [[ "${source_entry}" == *::* ]]; then
    printf '%s\n' "${source_entry%%::*}"
    return
  fi

  source_path="${source_entry%%#*}"
  printf '%s\n' "${source_path##*/}"
}

download_sources() {
  local destination="$1"
  shift

  mkdir -p "${destination}"

  local source_entry
  for source_entry in "$@"; do
    local url name
    url="$(source_url "${source_entry}")"
    name="$(source_name "${source_entry}")"
    echo "Downloading ${name}"
    curl -fL --retry 3 --retry-delay 2 -o "${destination}/${name}" "${url}"
  done
}

assert_path() {
  if [[ ! -e "$1" && ! -L "$1" ]]; then
    echo "expected path missing: $1" >&2
    exit 1
  fi
}

assert_symlink() {
  local path="$1"
  local target="$2"

  if [[ ! -L "${path}" ]]; then
    echo "expected symlink missing: ${path}" >&2
    exit 1
  fi

  local actual
  actual="$(readlink "${path}")"
  if [[ "${actual}" != "${target}" ]]; then
    echo "unexpected symlink target for ${path}: ${actual} != ${target}" >&2
    exit 1
  fi
}

assert_mode() {
  local path="$1"
  local expected="$2"
  local actual

  actual="$(stat -c '%a' "${path}")"
  if [[ "${actual}" != "${expected}" ]]; then
    echo "unexpected mode for ${path}: ${actual} != ${expected}" >&2
    exit 1
  fi
}

assert_package_layout() {
  local pkgdir="$1"
  local license_dir="$2"

  assert_path "${pkgdir}/opt/playit/agent"
  assert_path "${pkgdir}/opt/playit/playitd"
  assert_path "${pkgdir}/opt/playit/playit"
  assert_path "${pkgdir}/etc/logrotate.d/playit"
  assert_path "${pkgdir}/usr/lib/systemd/system/playit.service"
  assert_path "${pkgdir}/usr/lib/sysusers.d/playit.conf"
  assert_path "${pkgdir}/usr/share/licenses/${license_dir}/LICENSE.txt"
  assert_path "${pkgdir}/etc/playit"
  assert_symlink "${pkgdir}/usr/bin/playit" /opt/playit/playit
  assert_symlink "${pkgdir}/usr/bin/playitd" /opt/playit/playitd

  assert_mode "${pkgdir}/opt/playit/agent" 755
  assert_mode "${pkgdir}/opt/playit/playitd" 755
  assert_mode "${pkgdir}/opt/playit/playit" 755
  assert_mode "${pkgdir}/etc/logrotate.d/playit" 644
  assert_mode "${pkgdir}/usr/lib/systemd/system/playit.service" 644
  assert_mode "${pkgdir}/usr/lib/sysusers.d/playit.conf" 644
  assert_mode "${pkgdir}/etc/playit" 750
}

test_bin_pkgbuild() {
  echo "==> Testing arch/bin PKGBUILD"

  local test_dir srcdir pkgdir
  test_dir="$(mktemp -d)"
  srcdir="${test_dir}/src"
  pkgdir="${test_dir}/pkg"

  (
    set -euo pipefail
    # shellcheck disable=SC2034
    CARCH=x86_64
    # shellcheck disable=SC1091
    source arch/bin/PKGBUILD
    download_sources "${srcdir}" "${source[@]}" "${source_x86_64[@]}"
    package
    assert_package_layout "${pkgdir}" playit-bin
    file "${pkgdir}/opt/playit/agent"
    file "${pkgdir}/opt/playit/playitd"
  )

  rm -rf "${test_dir}"
}

extract_sources() {
  local source_dir="$1"
  shift

  local source_entry
  for source_entry in "$@"; do
    local name
    name="$(source_name "${source_entry}")"

    case "${name}" in
      *.tar|*.tar.gz|*.tgz|*.tar.bz2|*.tar.xz|*.tar.zst)
        tar -xf "${source_dir}/${name}" -C "${source_dir}"
        ;;
    esac
  done
}

test_build_pkgbuild() {
  echo "==> Testing arch/build PKGBUILD"

  local test_dir srcdir pkgdir
  test_dir="$(mktemp -d)"
  srcdir="${test_dir}/src"
  pkgdir="${test_dir}/pkg"
  mkdir -p "${srcdir}" "${pkgdir}"

  (
    set -euo pipefail
    # shellcheck disable=SC2034
    CARCH=x86_64
    # shellcheck disable=SC1091
    source arch/build/PKGBUILD

    download_sources "${srcdir}" "${source[@]}"
    extract_sources "${srcdir}" "${source[@]}"

    export CARGO_HOME="${test_dir}/cargo-home"

    prepare
    build
    package
    assert_package_layout "${pkgdir}" playit
    file "${pkgdir}/opt/playit/agent"
    file "${pkgdir}/opt/playit/playitd"
  )

  rm -rf "${test_dir}"
}

require_file arch/bin/PKGBUILD
require_file arch/bin/playit-bin.install
require_file arch/build/PKGBUILD
require_file arch/build/playit.install

bash -n arch/bin/PKGBUILD
bash -n arch/bin/playit-bin.install
bash -n arch/build/PKGBUILD
bash -n arch/build/playit.install

test_bin_pkgbuild
test_build_pkgbuild

echo "PKGBUILD tests passed"
