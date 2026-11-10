# Sign Windows releases

The release workflow builds the Windows executables and MSIs on GitHub Actions and uploads them unsigned.
Sign them afterwards from a Linux machine with the code-signing YubiKey connected:

```sh
build-scripts/windows-sign.sh 1.2.3
```

The script downloads these assets from the `v1.2.3` GitHub release for `x86` and `x86_64`:

| Asset | Signed output |
|---|---|
| `playit-windows-<arch>.exe` | `playit-windows-<arch>-signed.exe` |
| `playitd-windows-<arch>.exe` | `playitd-windows-<arch>-signed.exe` |
| `playit-windows-<arch>.msi` | `playit-windows-<arch>-signed.msi` |

It signs and timestamps each file with Jsign, checks the signature with `osslsigncode`, and writes the signed files and a `SHA256SUMS` file to `target/windows-sign/<version>/`.
The unsigned downloads are kept in `target/windows-sign/<version>/unsigned/`.
The script does not upload anything. Attach the `-signed` files to the release yourself.

Signing an MSI does not sign the programs inside it.
Programs installed from the MSI, including `playit-gui.exe`, stay unsigned.

## Requirements

Install Java, `osslsigncode`, Yubico's YKCS11 module, and PC/SC support.
On Debian 13:

```sh
sudo apt-get install openjdk-21-jre-headless osslsigncode ykcs11 yubico-piv-tool pcscd
sudo systemctl start pcscd.socket
```

The default certificate chain is `build-scripts/digicert.pem`.
Set `PLAYIT_SIGN_CERTIFICATE` to use a renewed certificate chain.

## PIN and certificate selection

The script asks for the PIN after all downloads succeed, without displaying it.
It passes the PIN to Jsign through an environment variable rather than a command-line argument.

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
Any download, signing, timestamping, or verification failure stops the script.

## Validate on Windows

```powershell
Get-ChildItem .\*-signed.exe, .\*-signed.msi |
    Get-AuthenticodeSignature |
    Format-Table Status, Path
```

Each file should report `Valid`.
