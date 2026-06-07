#!/usr/bin/env bash
set -euo pipefail

IMAGE="${PLAYIT_ARCH_TEST_IMAGE:-docker.io/library/alpine:latest}"
SCRIPT_PATH="${BASH_SOURCE[0]}"
REPO_DIR="$(cd -- "$(dirname -- "${SCRIPT_PATH}")/.." && pwd)"
PKGBUILD_PATH="arch/bin/PKGBUILD"
ARCHES=(x86_64 aarch64 armv7h i686)

usage() {
  cat >&2 <<EOF
usage: $0 [--inside-container]

Updates sha256sums in ${PKGBUILD_PATH} by downloading every source listed
by the base and architecture-specific source arrays.

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
    echo "podman is required to update PKGBUILD checksums" >&2
    exit 1
  fi

  exec podman run --rm \
    --volume "${REPO_DIR}:/work:Z" \
    --workdir /work \
    "${IMAGE}" \
    /bin/sh -ceu '
      apk add --no-cache bash ca-certificates coreutils curl
      exec bash arch/update-bin-pkgbuild-shasum.sh --inside-container
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

replace_sha256sums() {
  local pkgbuild="$1"
  local sums_file="$2"
  local tmp

  tmp="$(mktemp)"
  awk -v sums_file="${sums_file}" '
    BEGIN {
      while ((getline line < sums_file) > 0) {
        generated = generated line ORS
      }
      close(sums_file)
    }
    skip {
      if ($0 ~ /\)/) {
        skip = 0
      }
      next
    }
    /^sha256sums(_x86_64|_aarch64|_armv7h|_i686)?=\(/ {
      if (!inserted) {
        printf "%s", generated
        inserted = 1
      }
      if ($0 !~ /\)/) {
        skip = 1
      }
      next
    }
    { print }
  ' "${pkgbuild}" > "${tmp}"
  mv "${tmp}" "${pkgbuild}"
}

format_sha256sums() {
  local name="$1"
  shift

  printf '%s=(' "${name}"

  local index=0
  local sum
  for sum in "$@"; do
    if [[ "${index}" -gt 0 ]]; then
      printf ' '
    fi
    printf "'%s'" "${sum}"
    index=$((index + 1))
  done

  printf ')\n'
}

hash_source_array() {
  local output_name="$1"
  local source_array_name="$2"
  local -n source_entries="${source_array_name}"
  local sums=()
  local source_entry url sum

  for source_entry in "${source_entries[@]}"; do
    url="$(source_url "${source_entry}")"
    echo "Hashing ${url}" >&2
    sum="$(curl -fL --retry 3 --retry-delay 2 "${url}" | sha256sum | awk '{ print $1 }')"
    sums+=("${sum}")
  done

  format_sha256sums "${output_name}" "${sums[@]}"
}

generate_sha256sums() {
  local output="$1"
  local arch

  # shellcheck disable=SC1090
  source "${PKGBUILD_PATH}"

  {
    hash_source_array sha256sums source

    for arch in "${ARCHES[@]}"; do
      hash_source_array "sha256sums_${arch}" "source_${arch}"
    done
  } > "${output}"
}

require_file "${PKGBUILD_PATH}"

bash -n "${PKGBUILD_PATH}"

work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

generate_sha256sums "${work_dir}/sha256sums.out"

if [[ ! -s "${work_dir}/sha256sums.out" ]]; then
  echo "failed to produce sha256sums output" >&2
  exit 1
fi

replace_sha256sums "${PKGBUILD_PATH}" "${work_dir}/sha256sums.out"
bash -n "${PKGBUILD_PATH}"

echo "Updated ${PKGBUILD_PATH} sha256sums"
