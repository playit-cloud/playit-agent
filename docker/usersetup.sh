#!/usr/bin/env sh

if ! id "playit" >/dev/null 2>&1; then
  if [ -z "${PLAYIT_UUID}" ]; then
    PLAYIT_UUID=1000
  fi
  if [ -z "${PLAYIT_GUID}" ]; then
    PLAYIT_GUID=1000
  fi
  addgroup -g ${PLAYIT_GUID} playit
  adduser -Ss /sbin/nologin -u ${PLAYIT_UUID} -G playit -H playit
fi
