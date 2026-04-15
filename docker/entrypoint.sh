#!/usr/bin/env sh

SECRET_KEY="${SECRET_KEY:-}"

if [ -z "${SECRET_KEY}" ]; then
  SECRET_KEY="${1:-}"

  if [ -z "${SECRET_KEY}" ]; then
    echo "secret key is required via SECRET_KEY or the first argument" >&2
    exit 1
  fi

  shift
fi

exec playitd --secret "${SECRET_KEY}" --platform-docker "$@" 2>&1
