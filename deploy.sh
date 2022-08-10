# SHOULD BE RUN ON x86-64 linux machine

FOLDER=$(dirname "$0")
VERSION="$(toml get "${FOLDER}/packages/agent_cli/Cargo.toml" package.version | sed "s/\"//g")"

# build
bash ${FOLDER}/build-scripts/ubuntu-deb.sh
cross build --release --target x86_64-pc-windows-gnu --bin=agent
cargo build --release --bin=agent

rm -f build-deploy/*

cp ${FOLDER}/target/aarch64-unknown-linux-gnu/release/agent build-deploy/playit-${VERSION}-aarch64
cp ${FOLDER}/target/armv7-unknown-linux-gnueabihf/release/agent build-deploy/playit-${VERSION}-arm7
cp ${FOLDER}/target/release/agent build-deploy/playit-${VERSION}
cp ${FOLDER}/target/x86_64-pc-windows-gnu/release/agent.exe build-deploy/playit-${VERSION}-unsigned.exe
