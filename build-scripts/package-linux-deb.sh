cargo install toml-cli

START_DIR="$(pwd)"

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

SRC_PATH="$1"
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

ROOT_CARGO_FILE="${SCRIPT_DIR}/../Cargo.toml"
CARGO_FILE="${SCRIPT_DIR}/../packages/agent_cli/Cargo.toml"
VERSION=$(toml get "${ROOT_CARGO_FILE}" workspace.package.version | sed "s/\"//g")

DEB_PACKAGE="playit_${DEB_ARCH}"
# shellcheck disable=SC2164
mkdir "${DEB_PACKAGE}" && cd "${DEB_PACKAGE}"
WK_DIR=$(pwd)

# Copy over playit binary
echo "PREPARE BINARY AND RUN SCRIPT"
mkdir -p "${WK_DIR}${INSTALL_FOLDER}"

cp "${START_DIR}/${SRC_PATH}" "${WK_DIR}${INSTALL_FOLDER}/agent"

# Create run script
echo "#!/bin/bash
/opt/playit/agent \$@
" > "${WK_DIR}${INSTALL_FOLDER}/playit"
chmod 0755 "${WK_DIR}${INSTALL_FOLDER}/playit"

# Add systemd unit
mkdir -p "${WK_DIR}/lib/systemd/system"
cp "${SCRIPT_DIR}/../linux/playit.service" "${WK_DIR}/lib/systemd/system/playit.service"

# Add logrotate config
mkdir -p "${WK_DIR}/etc/logrotate.d"
cp "${SCRIPT_DIR}/../linux/logrotate.conf" "${WK_DIR}/etc/logrotate.d/playit"

# build control file
echo "BUILD DEB CONFIG FILES"

mkdir -p DEBIAN
echo "
Package: playit
Version: ${VERSION}
Architecture: ${DEB_ARCH}
Maintainer: $(toml get "${CARGO_FILE}" 'package.authors[0]' | sed "s/\"//g")
Description: $(toml get "${CARGO_FILE}" package.description | sed "s/\"//g")
Depends: logrotate
" > "${WK_DIR}/DEBIAN/control"

# setup script
cat <<EOF > "${WK_DIR}/DEBIAN/postinst"
#!/bin/bash
ln -s ${INSTALL_FOLDER}/playit /usr/local/bin/playit
mkdir -p /var/log/playit # make logs folder
chmod 0766 -R /var/log/playit
mkdir -p /etc/playit
chmod 0766 /etc/playit
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
dpkg-deb --build -Zgzip --root-owner-group "${DEB_PACKAGE}"

mkdir -p "${SCRIPT_DIR}/../target/deb"
cp "${DEB_PACKAGE}.deb" "${SCRIPT_DIR}/../target/deb"
