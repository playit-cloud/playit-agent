#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." &>/dev/null && pwd)"

VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "missing version" >&2
  exit 1
fi

if [ -f "${REPO_ROOT}/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  source "${REPO_ROOT}/.env"
  set +a
fi

if ! command -v s3cmd >/dev/null 2>&1; then
  echo "s3cmd is required but was not found in PATH" >&2
  exit 1
fi

REQUIRED_ENV=(
  R2_BUCKET
  R2_ACCOUNT_ID
  S3CMD_ACCESS_KEY
  S3CMD_SECRET_KEY
)

for name in "${REQUIRED_ENV[@]}"; do
  if [ -z "${!name:-}" ]; then
    echo "missing required environment variable: ${name}" >&2
    exit 1
  fi
done

"${SCRIPT_DIR}/download.sh" "$VERSION"

DOWNLOAD_DIR="${REPO_ROOT}/target/downloads"

if [ ! -d "$DOWNLOAD_DIR" ]; then
  echo "download directory does not exist: ${DOWNLOAD_DIR}" >&2
  exit 1
fi

if ! find "$DOWNLOAD_DIR" -type f -print -quit | grep -q .; then
  echo "download directory is empty: ${DOWNLOAD_DIR}" >&2
  exit 1
fi

R2_HOST="${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"

s3cmd \
  --access_key="${S3CMD_ACCESS_KEY}" \
  --secret_key="${S3CMD_SECRET_KEY}" \
  --host="${R2_HOST}" \
  --host-bucket="%(bucket)s.${R2_HOST}" \
  put \
  --recursive \
  "${DOWNLOAD_DIR}/" \
  "s3://${R2_BUCKET}/${VERSION}/"

echo "Uploaded release files to https://builds.playit.gg/${VERSION}/"
