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

start_agent() {
  docker run -d --name "$name" --network none -e SECRET_KEY=00 "$@" "$image" > "$test_dir/id"
  for attempt in $(seq 1 50); do
    docker logs "$name" > "$test_dir/stdout" 2> "$test_dir/stderr"
    if grep -q 'Starting playitd' "$test_dir/stdout"; then
      test ! -s "$test_dir/stderr"
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
  docker logs "$name" > "$test_dir/stdout" 2> "$test_dir/stderr"
  grep -q 'playitd shutdown complete' "$test_dir/stdout"
  docker rm "$name" > /dev/null
}

start_agent
test "$(docker inspect --format '{{.Config.Tty}} {{.Config.OpenStdin}}' "$name")" = 'false false'
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Docker stop without a terminal: passed'

start_agent -t
docker stop --timeout 2 "$name" > /dev/null
check_exit
echo 'Docker stop with a terminal: passed'

start_agent
docker kill --signal INT "$name" > /dev/null
check_exit
echo 'SIGINT shutdown: passed'
