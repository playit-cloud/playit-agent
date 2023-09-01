SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

BUILD_ARCH="$1"
DEB_ARCH="$2"

TEMP_DIR_NAME="temp-build-${DEB_ARCH}"

# prepare temp build folder
echo "PREPARE TEMP BUILD FOLDER"
# shellcheck disable=SC2164
cd "${SCRIPT_DIR}"
rm -fr "${TEMP_DIR_NAME}"
mkdir "${TEMP_DIR_NAME}"
# shellcheck disable=SC2164
cd "${TEMP_DIR_NAME}"

INSTALL_FOLDER="/opt/playit"

CARGO_FILE="${SCRIPT_DIR}/../packages/agent_cli/Cargo.toml"
VERSION=$(toml get "${CARGO_FILE}" package.version | sed "s/\"//g")

DEB_PACKAGE="playit_${DEB_ARCH}"
# shellcheck disable=SC2164
mkdir "${DEB_PACKAGE}" && cd "${DEB_PACKAGE}"
WK_DIR=$(pwd)

# Copy over playit binary
echo "PREPARE BINARY AND RUN SCRIPT"
mkdir -p "${WK_DIR}${INSTALL_FOLDER}"
cp "${SCRIPT_DIR}/../target/${BUILD_ARCH}/release/playit-cli" "${WK_DIR}${INSTALL_FOLDER}/agent"

# Create run script
echo "#!/bin/bash
/opt/playit/agent \$@
" > "${WK_DIR}${INSTALL_FOLDER}/playit"
chmod 0755 "${WK_DIR}${INSTALL_FOLDER}/playit"

# build control file
echo "BUILD DEB CONFIG FILES"

mkdir -p DEBIAN
echo "
Package: playit
Version: ${VERSION}
Architecture: ${DEB_ARCH}
Maintainer: $(toml get "${CARGO_FILE}" package.metadata.deb.maintainer | sed "s/\"//g")
Description: $(toml get "${CARGO_FILE}" package.description | sed "s/\"//g")
" > "${WK_DIR}/DEBIAN/control"

# setup script
cat <<EOF > "${WK_DIR}/DEBIAN/postinst"
#!/bin/bash
ln -s ${INSTALL_FOLDER}/playit /usr/local/bin/playit
mkdir -p /var/log/playit # make logs folder
chmod 757 -R /var/log/playit
mkdir -p /etc/playit # make configuration folder
chmod 757 -R /etc/playit
EOF
chmod 0555 "${WK_DIR}/DEBIAN/postinst"

# teardown script
cat <<EOF > "${WK_DIR}/DEBIAN/prerm"
#!/bin/bash
if [[ -L "/usr/local/bin/playit" ]]; then
  rm "/usr/local/bin/playit";
fi
EOF
chmod 0555 "${WK_DIR}/DEBIAN/prerm"

# build package
# shellcheck disable=SC2164
cd "${SCRIPT_DIR}/${TEMP_DIR_NAME}"
dpkg-deb --build --root-owner-group "${DEB_PACKAGE}"

mkdir -p "${SCRIPT_DIR}/../target/deb"
cp "${DEB_PACKAGE}.deb" "${SCRIPT_DIR}/../target/deb"
