# SHOULD BE RUN ON x86-64 linux machine

DEPLOY_TARGET=$1
FOLDER=$(dirname "$0")
REMOTE_PATH=/var/www/playit/downloads

VERSION="$(toml get "${FOLDER}/packages/agent/Cargo.toml" package.version | sed "s/\"//g")"
echo "deploying version ${VERSION} to ${DEPLOY_TARGET}:${REMOTE_PATH}"

# build
bash ${FOLDER}/build-scripts/ubuntu-deb.sh
cross build --release --target x86_64-pc-windows-gnu --bin=agent
cargo build --release --bin=agent

# upload
scp ${FOLDER}/target/aarch64-unknown-linux-gnu/release/agent ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}-aarch64
scp ${FOLDER}/target/armv7-unknown-linux-gnueabihf/release/agent ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}-arm7
scp ${FOLDER}/target/release/agent ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}
scp ${FOLDER}/target/x86_64-pc-windows-gnu/release/agent.exe ${DEPLOY_TARGET}:${REMOTE_PATH}/playit-${VERSION}.exe
