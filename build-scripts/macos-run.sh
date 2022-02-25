#!/usr/bin/env bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

mkdir -p ~/.config/playit
mkdir -p ~/Library/Logs/playit

cd ~/.config/playit
CONFIG_PATH=$(pwd)

cd ~/Library/Logs/playit
LOGS_PATH=$(pwd)

if [[ $(uname -p) == 'arm' ]]; then
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-m1 --config-file=${CONFIG_PATH}/playit.toml --log-folder=${LOGS_PATH}/playit\"
  end tell"
else
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-intel --config-file=${CONFIG_PATH}/playit.toml --log-folder=${LOGS_PATH}/playit\"
  end tell"
fi

