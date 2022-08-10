# SHOULD BE RUN ON M1 MAC

FOLDER=$(dirname "$0")
VERSION="$(toml get "${FOLDER}/packages/agent_cli/Cargo.toml" package.version | sed "s/\"//g")"

bash ${FOLDER}/build-scripts/macos-app.sh

mkdir -p ${FOLDER}/build-deploy

cp ${FOLDER}/build-scripts/out/playit.dmg ${FOLDER}/build-deploy/playit-${VERSION}.dmg
cp ${FOLDER}/target/release/agent ${FOLDER}/build-deploy/playit-${VERSION}-apple-m1
cp ${FOLDER}/target/x86_64-apple-darwin/release/agent ${FOLDER}/build-deploy/playit-${VERSION}-apple-intel
