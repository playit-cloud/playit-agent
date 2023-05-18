SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

CROSS_ARCH="$1"
ARCH="$2"

cross build --release --bin=playit-cli --target="${CROSS_ARCH}"

TEMP_DIR_NAME="temp-build-${ARCH}"

# prepare temp build folder
echo "PREPARE TEMP BUILD FOLDER"
cd "${SCRIPT_DIR}"
rm -fr "${TEMP_DIR_NAME}"
mkdir "${TEMP_DIR_NAME}"
cd "${TEMP_DIR_NAME}"

INSTALL_FOLDER="/opt/playit"

CARGO_FILE="${SCRIPT_DIR}/../packages/agent_cli/Cargo.toml"
VERSION=$(toml get "${CARGO_FILE}" package.version | sed "s/\"//g")

DEB_PACKAGE="playit_${VERSION}_${ARCH}"
mkdir "${DEB_PACKAGE}" && cd "${DEB_PACKAGE}"
WK_DIR=$(pwd)

# Copy over playit binary
echo "PREPARE BINARY AND RUN SCRIPT"
mkdir -p "${WK_DIR}${INSTALL_FOLDER}"
cp "${SCRIPT_DIR}/../target/${CROSS_ARCH}/release/playit-cli" "${WK_DIR}${INSTALL_FOLDER}/agent"

# Create run script
echo "#!/bin/bash
/opt/playit/agent --use-linux-path-defaults \$@
" > "${WK_DIR}${INSTALL_FOLDER}/playit"
chmod 0755 "${WK_DIR}${INSTALL_FOLDER}/playit"

# build control file
echo "BUILD DEB CONFIG FILES"

mkdir -p DEBIAN
echo "
Package: playit
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: $(toml get "${CARGO_FILE}" package.metadata.deb.maintainer | sed "s/\"//g")
Description: $(toml get "${CARGO_FILE}" package.description | sed "s/\"//g")
" > "${WK_DIR}/DEBIAN/control"

# setup script
echo "#!/bin/bash
ln -s ${INSTALL_FOLDER}/playit /usr/local/bin/playit
mkdir -p /var/log/playit # make logs folder
chmod 757 -R /var/log/playit
mkdir -p /etc/playit # make configuration folder
chmod 757 -R /etc/playit
" > "${WK_DIR}/DEBIAN/postinst"
chmod 0555 "${WK_DIR}/DEBIAN/postinst"

# teardown script
echo "#!/bin/bash" > "${WK_DIR}/DEBIAN/prerm"
cat "${SCRIPT_DIR}/delete-playit-symlink.sh" >> "${WK_DIR}/DEBIAN/prerm"
chmod 0555 "${WK_DIR}/DEBIAN/prerm"

# build package
cd "${SCRIPT_DIR}/${TEMP_DIR_NAME}"
dpkg-deb --build --root-owner-group "${DEB_PACKAGE}"
cp "${DEB_PACKAGE}.deb" "${SCRIPT_DIR}/out/"

