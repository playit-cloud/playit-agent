#!/bin/sh
set -eu

case "${1:-}" in
  -h|--help) exec playitd "$@" 2>&1 ;;
esac

SECRET_KEY="${SECRET_KEY:-}"
SECRET_KEY_FILE="${SECRET_KEY_FILE:-}"

if [ -n "$SECRET_KEY_FILE" ]; then
  if [ -n "$SECRET_KEY" ]; then
    echo 'Set only one of SECRET_KEY or SECRET_KEY_FILE' >&2
    exit 1
  fi
  if [ ! -f "$SECRET_KEY_FILE" ] || [ ! -r "$SECRET_KEY_FILE" ]; then
    echo 'SECRET_KEY_FILE must point to a readable file containing the secret key' >&2
    exit 1
  fi
  SECRET_KEY="$(cat -- "$SECRET_KEY_FILE")"
elif [ -z "$SECRET_KEY" ]; then
  case "${1:-}" in
    ''|-*)
      echo 'Set SECRET_KEY or SECRET_KEY_FILE, or pass the secret key as the first argument' >&2
      exit 1
      ;;
    *) SECRET_KEY="$1"; shift ;;
  esac
fi

if [ -z "$SECRET_KEY" ]; then
  echo 'The secret key must not be empty' >&2
  exit 1
fi

# TLS certificates for HTTPS tunnels live on the /var/lib/playit volume so they survive
# container upgrades. An explicit --cert-dir argument takes precedence.
cert_dir_arg="--cert-dir=${CERT_DIR:-/var/lib/playit/certs}"
for arg in "$@"; do
  case "$arg" in
    --cert-dir|--cert-dir=*) cert_dir_arg='' ;;
  esac
done

exec playitd --secret="$SECRET_KEY" --platform-docker ${cert_dir_arg:+"$cert_dir_arg"} "$@" 2>&1
