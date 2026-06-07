#!/bin/sh
set -eu

PLAYIT_USER=playit
PLAYIT_HOME=/nonexistent
SYSUSERS_SHELL=/usr/sbin/nologin

have_command() {
  command -v "$1" >/dev/null 2>&1
}

if ! have_command systemd-sysusers; then
  echo "Cannot provision ${PLAYIT_USER} user with systemd-sysusers: command not found" >&2
  exit 1
fi

systemd-sysusers --inline "u ${PLAYIT_USER} - \"playit service user\" ${PLAYIT_HOME} ${SYSUSERS_SHELL}"
