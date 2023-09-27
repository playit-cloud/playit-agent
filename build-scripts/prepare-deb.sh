SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

set -e

cargo build --release --target=x86_64-unknown-linux-musl
cargo build --release --target=aarch64-unknown-linux-gnu
cargo build --release --target=armv7-unknown-linux-gnueabihf
cargo build --release --target=i686-unknown-linux-gnu
cargo build --release --target=mipsel-unknown-linux-gnu
cargo build --release --target=mips-unknown-linux-gnu

bash "${SCRIPT_DIR}/package-linux-deb.sh" x86_64-unknown-linux-musl amd64
bash "${SCRIPT_DIR}/package-linux-deb.sh" aarch64-unknown-linux-gnu arm64
bash "${SCRIPT_DIR}/package-linux-deb.sh" armv7-unknown-linux-gnueabihf armhf
bash "${SCRIPT_DIR}/package-linux-deb.sh" i686-unknown-linux-gnu i386
bash "${SCRIPT_DIR}/package-linux-deb.sh" mipsel-unknown-linux-gnu mipsel
bash "${SCRIPT_DIR}/package-linux-deb.sh" mips-unknown-linux-gnu mips

ROOT_CARGO_FILE="${SCRIPT_DIR}/../Cargo.toml"
VERSION=$(toml get "${ROOT_CARGO_FILE}" workspace.package.version | sed "s/\"//g")

cp target/deb/playit_amd64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_amd64.deb"
cp target/deb/playit_arm64.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_arm64.deb"
cp target/deb/playit_armhf.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_armhf.deb"
cp target/deb/playit_i386.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_i386.deb"
cp target/deb/playit_mipsel.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_mipsel.deb"
cp target/deb/playit_mips.deb "${SCRIPT_DIR}/../../ppa/data/playit_${VERSION}_mips.deb"
