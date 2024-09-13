#!/usr/bin/env sh

if [ -z "${SECRET_KEY}" ]; then
  echo "SECRET_KEY environment variable missing, using CLI argument";
  SECRET_KEY="$1"

  if [ -z "${SECRET_KEY}" ]; then
    echo "secret key (first argument) is missing";
    exit 1;
  fi
fi

term_handler() {

  echo "Shutting down Playit."
  kill -SIGTERM $PlayitPID
  wait $PlayitPID
	exit
}
trap 'term_handler' SIGTERM


playit -s --secret "${SECRET_KEY}" --platform_docker start &

PlayitPID=$!

wait $PlayitPID