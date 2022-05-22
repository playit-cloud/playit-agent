SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

echo "building arm64 .deb"
echo "---------------------"
bash "${SCRIPT_DIR}/build-linux-deb.sh" "aarch64-unknown-linux-gnu" "arm64"

echo "building armhf .deb"
echo "---------------------"
bash "${SCRIPT_DIR}/build-linux-deb.sh" "armv7-unknown-linux-gnueabihf" "armhf"

echo "building amd64 .deb"
echo "---------------------"
bash "${SCRIPT_DIR}/build-linux-deb.sh" "x86_64-unknown-linux-gnu" "amd64"
