# Sign Windows releases

The release workflow builds the Windows programs and an unsigned MSI on GitHub Actions and uploads them to the release.
Sign them afterwards from a Linux machine with the code-signing YubiKey connected:

```sh
build-scripts/windows-sign.sh 1.2.3
```

For `x86_64` and `x86`, the script:

1. Downloads the four unsigned programs from the `v1.2.3` release.
2. Signs and timestamps each program with Jsign, then checks the signature with `osslsigncode`.
3. Packages the signed programs into an MSI with WiX 3.14.1 under Wine in Docker.
4. Checks that the MSI contains the signed programs byte for byte.
5. Signs and checks the MSI.

The MSI is built the way `cargo wix` builds it in CI: the same `main.wxs`, `en-us.wxl`, WiX extensions, and variables.
The WiX sources come from the `v1.2.3` tag, not the current checkout.
WiX's ICE validation needs Windows, so it is skipped. Test the installer on Windows before releasing it.

The script does not upload anything.
It writes the following files and a `SHA256SUMS` file to `target/windows-sign/<version>/`:

| Release asset downloaded | Signed output |
|---|---|
| `playit-windows-<arch>.exe` | `playit-windows-<arch>-signed.exe` |
| `playitd-windows-<arch>.exe` | `playitd-windows-<arch>-signed.exe` |
| `playitd-tray-windows-<arch>.exe` | `playitd-tray-windows-<arch>-signed.exe` |
| `playit-gui-windows-<arch>.exe` | `playit-gui-windows-<arch>-signed.exe` |
| (built locally) | `playit-windows-<arch>-signed.msi` |

Attach the `-signed` files to the release.
Releases from before CI uploaded the tray and GUI programs lack them.
For those releases, put all eight programs in a folder and pass it with `--from DIR`.

To test packaging without the YubiKey, add `--skip-sign`.
The outputs then have no `-signed` suffix.

## Requirements

Install Docker and git. The current user must be able to run containers, or have passwordless sudo for Docker.
The first run builds the packaging image, and later runs reuse it.

For signing, install Java, `osslsigncode`, Yubico's YKCS11 module and PIV tool, and PC/SC support.
On Debian 13:

```sh
sudo apt-get install openjdk-21-jre-headless osslsigncode ykcs11 yubico-piv-tool pcscd
sudo systemctl start pcscd.socket
```

The default certificate chain is `build-scripts/digicert.pem`.
Set `PLAYIT_SIGN_CERTIFICATE` to use a renewed certificate chain.

## Troubleshooting

If jsign fails with `slotListIndex is 0 but token only has 0 slots`, the YubiKey PKCS#11 module cannot see the key.
The script checks for this with `yubico-piv-tool -a status` before asking for the PIN.
Plug in the YubiKey, start `pcscd.socket`, and run `gpgconf --kill scdaemon` if GnuPG is holding the card.

## PIN and certificate selection

The script asks for the PIN after all downloads succeed, without displaying it.
It passes the PIN to Jsign through an environment variable rather than a command-line argument.
The PIN never enters the packaging container.

To read the PIN from a private file instead:

```sh
chmod 600 /path/to/yubikey-pin
PLAYIT_SIGN_PIN_FILE=/path/to/yubikey-pin build-scripts/windows-sign.sh 1.2.3
```

If the token contains multiple certificates, select the certificate alias:

```sh
PLAYIT_SIGN_ALIAS='X.509 Certificate for Digital Signature' build-scripts/windows-sign.sh 1.2.3
```

Set `PLAYIT_TIMESTAMP_URL` to override the RFC3161 timestamp server.
The default is `http://timestamp.digicert.com`.
Any download, signing, timestamping, packaging, or verification failure stops the script.

## Validate on Windows

```powershell
Get-ChildItem .\*-signed.exe, .\*-signed.msi |
    Get-AuthenticodeSignature |
    Format-Table Status, Path

msiexec /i playit-windows-x86_64-signed.msi
Get-AuthenticodeSignature 'C:\Program Files\playit_gg\bin\*.exe' |
    Format-Table Status, Path
```

Each file should report `Valid`.
