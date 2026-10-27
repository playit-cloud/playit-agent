#!/usr/bin/env bash

log() {
  TZ=UTC printf '[windows-build %(%H:%M:%SZ)T] %s\n' -1 "$*"
}

run_step() (
  label="$1"
  shift
  started=$SECONDS
  log "$label"
  (
    while sleep 15; do
      log "$label: still running ($((SECONDS - started))s elapsed)"
    done
  ) 9>&- &
  heartbeat=$!
  trap 'kill "$heartbeat" 2>/dev/null || true; wait "$heartbeat" 2>/dev/null || true' EXIT

  status=0
  "$@" || status=$?
  if [[ "$status" = 0 ]]; then
    log "$label: done ($((SECONDS - started))s)"
  else
    log "$label: failed (exit $status, $((SECONDS - started))s)"
  fi
  exit "$status"
)
