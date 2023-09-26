SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

set -e

cargo build --release --target=x86_64-unknown-linux-musl
cargo build --release --target=aarch64-unknown-linux-gnu
cargo build --release --target=armv7-unknown-linux-gnueabihf

bash "${SCRIPT_DIR}/package-linux-deb.sh" x86_64-unknown-linux-musl amd64
bash "${SCRIPT_DIR}/package-linux-deb.sh" aarch64-unknown-linux-gnu arm64
bash "${SCRIPT_DIR}/package-linux-deb.sh" armv7-unknown-linux-gnueabihf armhf

ROOT_CARGO_FILE="${SCRIPT_DIR}/../Cargo.toml"
VERSION=$(toml get "${ROOT_CARGO_FILE}" workspace.package.version | sed "s/\"//g")

cp target/deb/playit_amd64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_amd64.deb"
cp target/deb/playit_arm64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_arm64.deb"
cp target/deb/playit_armhf.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_armhf.deb"
