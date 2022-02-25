#!/usr/bin/env bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

if [[ $(uname -p) == 'arm' ]]; then
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-m1\"
  end tell"
else
  osascript -e "tell app \"Terminal\"
    do script \" ${SCRIPT_DIR}/agent-intel\"
  end tell"
fi

