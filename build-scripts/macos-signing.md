# Sign macOS releases on Linux

A Mac builds the macOS release unsigned.
A Linux machine signs, notarizes, and staples it.
The signing keys and the App Store Connect API key never need to be on the Mac.

The Mac is still necessary for the build.
The build uses `lipo`, `iconutil`, `pkgbuild`, and `productbuild`, which only exist on macOS.

## Summary

On the Mac:

```sh
build-scripts/package-macos.sh
```

Do not set the `PLAYIT_MACOS_*` signing variables on the Mac.
Copy `target/macos-release/<version>/` to the same path in the Linux checkout:

```sh
rsync -a mac:playit-agent/target/macos-release/ target/macos-release/
```

On Linux, one time:

```sh
sudo apt-get install xar
build-scripts/macos/install-signing-tools.sh        # .p12 or PEM keys
build-scripts/macos/install-signing-tools.sh --smartcard   # YubiKey keys
```

On Linux, for each release:

```sh
export PLAYIT_MACOS_APP_P12=~/keys/developer-id-application.p12
export PLAYIT_MACOS_APP_P12_PASSWORD_FILE=~/keys/application-password
export PLAYIT_MACOS_INSTALLER_P12=~/keys/developer-id-installer.p12
export PLAYIT_MACOS_INSTALLER_P12_PASSWORD_FILE=~/keys/installer-password
export PLAYIT_MACOS_NOTARY_API_KEY=~/keys/app-store-connect.json
build-scripts/sign-macos.sh
```

The signed files are in `target/macos-release/<version>/signed/`.
Run `build-scripts/sign-macos.sh --help` for all options.

## Tools

`install-signing-tools.sh` puts these tools in `target/macos-sign-tools/bin`.
`sign-macos.sh` finds them there.

| Tool | Source | Use |
|---|---|---|
| `rcodesign` | [apple-codesign](https://gregoryszorc.com/docs/apple-codesign/main/) 0.29.0 | Signs code and the installer, notarizes, staples |
| `mkbom` | [bomutils](https://github.com/hogliux/bomutils), pinned commit | Writes the installer's bill of materials |
| `xar` | Debian package `xar` | Expands and creates the flat `.pkg` archive |

By default, the script downloads the prebuilt `rcodesign` and checks its SHA-256.
The prebuilt binary cannot use a YubiKey.
With `--smartcard`, the script compiles `rcodesign` with YubiKey support instead.
That build needs Rust, `libpcsclite-dev`, and `pcscd`:

```sh
sudo apt-get install libpcsclite-dev pcscd
sudo systemctl start pcscd.socket
```

## Certificates

Apple issues two Developer ID certificates for distribution outside the App Store:

* **Developer ID Application** signs the binaries and the app bundles.
* **Developer ID Installer** signs the `.pkg`.

Only the Apple Developer account holder can create Developer ID certificates.
For each certificate, set exactly one of the variables in this table:

| Key storage | Application certificate | Installer certificate |
|---|---|---|
| `.p12` file | `PLAYIT_MACOS_APP_P12` | `PLAYIT_MACOS_INSTALLER_P12` |
| PEM file (key and certificate) | `PLAYIT_MACOS_APP_PEM` | `PLAYIT_MACOS_INSTALLER_PEM` |
| YubiKey PIV slot | `PLAYIT_MACOS_APP_SMARTCARD_SLOT` | `PLAYIT_MACOS_INSTALLER_SMARTCARD_SLOT` |

If a `.p12` file has a password, put it in a private file.
Set `PLAYIT_MACOS_APP_P12_PASSWORD_FILE` or `PLAYIT_MACOS_INSTALLER_P12_PASSWORD_FILE` to that file.

### Export existing certificates as .p12

If the certificates are in a Mac keychain, do these steps on that Mac:

1. Open Keychain Access.
2. Select both "Developer ID" certificates in **My Certificates**.
3. Choose **File > Export Items** and save as Personal Information Exchange (`.p12`).
4. Copy the files to the Linux machine with `chmod 600`.

### Put the keys on a YubiKey

The YubiKey can hold the Apple keys in addition to the Windows signing key.
Use PIV slots that the Windows key does not use.
To see which slots hold certificates, run `rcodesign smartcard-scan`.
The examples use slot `9a` for the application certificate and `9d` for the installer certificate.

To create new keys on the YubiKey, so that the private keys never leave it:

```sh
rcodesign smartcard-generate-key --smartcard-slot 9a
rcodesign generate-certificate-signing-request --smartcard-slot 9a --csr-pem-file application.csr
rcodesign smartcard-generate-key --smartcard-slot 9d
rcodesign generate-certificate-signing-request --smartcard-slot 9d --csr-pem-file installer.csr
```

Upload each CSR at <https://developer.apple.com/account/resources/certificates/add>.
Choose "Developer ID Application" for `application.csr` and "Developer ID Installer" for `installer.csr`.
Import each downloaded certificate into the slot that holds its key:

```sh
rcodesign smartcard-import --existing-key --smartcard-slot 9a --certificate-der-file developerID_application.cer
rcodesign smartcard-import --existing-key --smartcard-slot 9d --certificate-der-file developerID_installer.cer
```

To move an existing `.p12` to the YubiKey instead:

```sh
rcodesign smartcard-import --smartcard-slot 9a --p12-file developer-id-application.p12 \
  --p12-password-file ~/keys/application-password
```

`rcodesign` cannot change the YubiKey management key.
If the key uses a custom management key, import fails, but signing still works.

Then sign with:

```sh
export PLAYIT_MACOS_APP_SMARTCARD_SLOT=9a
export PLAYIT_MACOS_INSTALLER_SMARTCARD_SLOT=9d
build-scripts/sign-macos.sh
```

The script asks for the PIN one time, without showing it.
It gives the PIN to `rcodesign` in an environment variable, not on the command line.
To read the PIN from a private file, set `PLAYIT_MACOS_SMARTCARD_PIN_FILE`.

## Notarization key

Notarization uses an App Store Connect API key, not an Apple ID password.

1. Open <https://appstoreconnect.apple.com/access/integrations/api>.
2. Create a team key with the Developer role.
3. Download `AuthKey_<KEY ID>.p8`. Apple lets you download it only one time.
4. Write down the issuer ID and the key ID.

Combine the three values into one JSON file:

```sh
rcodesign encode-app-store-connect-api-key -o ~/keys/app-store-connect.json \
  <ISSUER ID> <KEY ID> ~/Downloads/AuthKey_<KEY ID>.p8
chmod 600 ~/keys/app-store-connect.json
```

Set `PLAYIT_MACOS_NOTARY_API_KEY` to that file.
Notarization usually takes a few minutes.
If Apple rejects the submission, run `rcodesign notary-log --api-key-file <FILE> <SUBMISSION ID>` to see why.

## What sign-macos.sh does

1. Expands the `.pkg` with `xar` and unpacks its payload.
2. Signs `playit` and `playitd` with the application certificate and the hardened runtime.
3. Signs `playit-tray.app`, then `playit.app`, inside out, as `package-macos.sh` does on a Mac.
4. Puts the signed `playit` and `playitd` from the app into the tarball.
5. Repacks the payload and writes a new bill of materials and new size totals.
6. Rebuilds the `.pkg` with `xar` and signs it with the installer certificate.
7. Submits the `.pkg` for notarization, waits, and staples the ticket.
8. Copies the `.pkg`, the tarball, and `SHA256SUMS` to `signed/`.

The repacked payload keeps the owner, mode, and time of each original entry.
New entries, such as `_CodeSignature/CodeResources`, take the owner of their parent directory.

Apple cannot notarize a `.tar.gz`, so the tarball is not submitted separately.
Its `playit` and `playitd` are byte-identical to the copies in the app.
Notarizing the `.pkg` therefore registers their code hashes too.
Gatekeeper finds that registration online, because a ticket cannot be stapled to a bare binary.
The script stops if the tarball and the `.pkg` come from different builds.

A failure at any step stops the script.
It never writes unsigned files to `signed/`.

## Test without Apple certificates

A self-signed certificate checks the whole pipeline except notarization:

```sh
rcodesign generate-self-signed-certificate --person-name "playit test" \
  --pem-unified-file /tmp/test-cert.pem
PLAYIT_MACOS_APP_PEM=/tmp/test-cert.pem PLAYIT_MACOS_INSTALLER_PEM=/tmp/test-cert.pem \
  build-scripts/sign-macos.sh --skip-notarize
```

Gatekeeper rejects the result, because Apple did not issue the certificate.

## Verify on a Mac

Copy `signed/` to a Mac and run:

```sh
pkgutil --check-signature playit-<version>.pkg
spctl --assess --type install -vv playit-<version>.pkg
xcrun stapler validate playit-<version>.pkg
pkgutil --expand-full playit-<version>.pkg expanded
codesign --verify --deep --strict -vv expanded/playit-component.pkg/Payload/Applications/playit.app
```

A release build reports these results:

* `pkgutil` reports "signed by a developer certificate issued by Apple for distribution".
* `spctl` reports "accepted" with "source=Notarized Developer ID".
* `stapler` reports "The validate action worked".
* `codesign` reports "valid on disk" and "satisfies its Designated Requirement".

Then install the `.pkg` and open playit from Applications.
Also download `playit-macos-universal.tar.gz` in a browser, extract it, and run `./playit version`.
A browser download gets the quarantine attribute, so this tests Gatekeeper on the standalone binaries.

## Limits

* The payload must use `pkgbuild`'s default gzip compression. The script rejects `pbzx` payloads.
* The payload must not contain hard links.
* `rcodesign verify` is unreliable, so the script does not verify signatures on Linux. Notarization and the Mac checks above verify them.
