# SHOULD BE RUN ON M1 MAC

DEPLOY_TARGET=$1
FOLDER=$(dirname "$0")
REMOTE_PATH=/var/www/playit/downloads

VERSION="$(toml get "${FOLDER}/packages/agent_cli/Cargo.toml" package.version | sed "s/\"//g")"

bash ${FOLDER}/build-scripts/macos-app.sh
scp ${FOLDER}/build-scripts/out/playit.dmg ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}.dmg
scp ${FOLDER}/target/release/agent ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}-apple-m1
scp ${FOLDER}/target/x86_64-apple-darwin/release/agent ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}-apple-intel
