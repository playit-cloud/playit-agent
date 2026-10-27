#!/bin/sh
set -eu

image="${1:-playit-agent:docker-test}"
name="playit-docker-test-$$"
test_dir="$(mktemp -d)"

if docker container inspect "$name" >/dev/null 2>&1; then
  echo "Container $name already exists" >&2
  exit 1
fi

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  rm -rf "$test_dir"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

check_logs() {
  docker logs "$name" > "$test_dir/stdout" 2> "$test_dir/stderr"
  test ! -s "$test_dir/stderr"
  if LC_ALL=C grep -q "$(printf '\033')" "$test_dir/stdout"; then
    echo 'Container logs contain terminal escape sequences' >&2
    exit 1
  fi
  if grep -q 'IPC socket group is missing' "$test_dir/stdout"; then
    echo 'The runtime image is missing the IPC socket group' >&2
    exit 1
  fi
}

start_agent() {
  docker run -d --name "$name" --network none "$@" > "$test_dir/id"
  for attempt in $(seq 1 50); do
    docker logs "$name" > "$test_dir/stdout" 2> "$test_dir/stderr"
    if grep -q 'Starting playitd' "$test_dir/stdout"; then
      check_logs
      test "$(docker exec "$name" cat /proc/1/comm)" = playitd
      return
    fi
    sleep 0.1
  done
  echo "The container did not log its startup" >&2
  exit 1
}

check_exit() {
  code="$(timeout 5 docker wait "$name")"
  test "$code" = 0
  check_logs
  grep -q 'playitd shutdown complete' "$test_dir/stdout"
  docker rm "$name" > /dev/null
}

start_agent -e SECRET_KEY=00 "$image"
test "$(docker inspect --format '{{.Config.Tty}} {{.Config.OpenStdin}}' "$name")" = 'false false'
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Docker stop without a terminal: passed'

start_agent -t -e SECRET_KEY=00 "$image"
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Docker stop with a terminal: passed'

start_agent -e SECRET_KEY=00 "$image"
docker kill --signal INT "$name" > /dev/null
check_exit
echo 'SIGINT shutdown: passed'

timeout 10 docker run --rm --network none "$image" --help > "$test_dir/help"
grep -q 'Usage: playitd' "$test_dir/help"
echo 'Help without credentials: passed'

expect_error() {
  expected="$1"
  shift
  status=0
  timeout 10 docker run --rm --network none "$@" > "$test_dir/error" 2>&1 || status=$?
  test "$status" = 1
  grep -q "$expected" "$test_dir/error"
}

expect_error 'Set SECRET_KEY or SECRET_KEY_FILE' "$image"
expect_error 'Set SECRET_KEY or SECRET_KEY_FILE' "$image" --socket-path /tmp/playit.sock
expect_error 'Invalid secret passed via --secret' -e SECRET_KEY=not-a-key "$image"
expect_error 'Set only one' -e SECRET_KEY=00 -e SECRET_KEY_FILE=/missing "$image"
expect_error 'readable file' -e SECRET_KEY_FILE=/missing "$image"
touch "$test_dir/empty"
expect_error 'must not be empty' -v "$test_dir:/run/test:ro" -e SECRET_KEY_FILE=/run/test/empty "$image"
echo 'Configuration errors fail promptly: passed'

printf '00\n' > "$test_dir/secret"
start_agent -v "$test_dir:/run/test:ro" -e SECRET_KEY_FILE=/run/test/secret "$image"
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Read-only secret file: passed'

start_agent "$image" 00 --socket-path /tmp/playit-test.sock
docker exec "$name" test -S /tmp/playit-test.sock
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Legacy secret argument and daemon options: passed'
