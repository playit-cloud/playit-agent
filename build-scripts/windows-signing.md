# Build and sign Windows releases on Linux

Run the complete build from an x86_64 Linux machine:

```sh
./windows-build.sh
```

The script builds the x64 and x86 executables, signs them with the local YubiKey, packages the MSIs, then signs each MSI.
It never publishes a release or changes the version.
The version comes from the workspace `Cargo.toml`.

For testing without the YubiKey, run:

```sh
./windows-build.sh --skip-sign
```

Select one architecture with `--arch x86_64` or `--arch x86`.
Run `./windows-build.sh --help` for all options.
Progress messages go to stdout with UTC timestamps and elapsed times.
Long-running stages print a status line every 15 seconds, including during SDK downloads, compilation, signing, and Wine shutdown.

## Requirements

Install Docker on the Linux machine.
The current user must have permission to run containers.
If passwordless sudo is available, the script uses it for Docker while signing as the current user.
You can also run `sudo ./windows-build.sh --skip-sign`; generated files retain the invoking user's ownership.

For signed builds, install Java, `osslsigncode`, Yubico's YKCS11 module, and PC/SC support on the host.
On Debian 13:

```sh
sudo apt-get install openjdk-21-jre-headless osslsigncode ykcs11 yubico-piv-tool pcscd
sudo systemctl start pcscd.socket
```

Connect the YubiKey containing the code-signing key and certificate.
The default certificate chain is `build-scripts/digicert.pem`.
Set `PLAYIT_SIGN_CERTIFICATE` to use a renewed certificate chain.

The first build downloads the compiler image, Rust targets, Microsoft CRT, and Windows SDK.
[cargo-xwin](https://github.com/rust-cross/cargo-xwin) downloads the Microsoft libraries under their linked license.
Subsequent builds reuse the container layers and `target/windows-build` cache.

The container contains:

- Rust 1.99.1 and cargo-xwin 0.23.1 for MSVC cross-compilation.
- LLVM's native resource compiler for the executable icons, versions, and tray manifest.
- WiX 3.14.1 under Wine and Wine Mono 9.4.0 for MSI packaging.
- msitools for extracting and checking the packaged executables.

The WiX and Wine Mono downloads have pinned SHA-256 checksums.
Signing runs on the Linux host, so the container needs no USB access or PIN.

## PIN and certificate selection

The script prompts for the PIN without displaying it.
It passes the PIN to Jsign through an environment variable rather than a command-line argument.
The PIN is not passed into build containers.

For an existing private PIN file:

```sh
chmod 600 /path/to/yubikey-pin
PLAYIT_SIGN_PIN_FILE=/path/to/yubikey-pin ./windows-build.sh
```

If the token contains multiple certificates, select the certificate alias:

```sh
PLAYIT_SIGN_ALIAS='X.509 Certificate for Digital Signature' ./windows-build.sh
```

Set `PLAYIT_TIMESTAMP_URL` to override the RFC3161 timestamp server.
The default is `http://timestamp.digicert.com`.
A signing, timestamping, or verification failure stops the build.
There is no automatic fallback to unsigned artifacts.

## Artifacts

Outputs are separated by signing mode:

```text
target/windows-release/1.1.0/signed/
target/windows-release/1.1.0/unsigned/
```

Each directory contains these files for `x86_64` and `x86`:

| File | Purpose |
|---|---|
| `playit-windows-<arch>.exe` | Portable CLI; also attaches to an installed service |
| `playitd-windows-<arch>.exe` | Background daemon |
| `playitd-tray-windows-<arch>.exe` | Tray application |
| `playit-windows-<arch>.msi` | Installer containing all three programs |
| `SHA256SUMS` | Hashes of the completed artifacts |

Signed artifacts use the same filenames as unsigned artifacts, in separate directories.
Signed builds also include byte-identical `-signed` aliases for compatibility with the release download scripts.
Only upload artifacts from the `signed` directory for a release.
The script replaces the selected output directory's Windows artifacts after all requested architectures pass verification.

Compilation uses a separate target directory and does not replace native Linux build outputs.
Signing changes staged copies, leaving Cargo's unsigned executable cache intact.
Concurrent pipeline runs in the same checkout are rejected.

## Signing and verification order

The pipeline performs these steps:

1. Compile the CLI, daemon, and tray application for the selected architecture.
2. Copy the three executables into a staging directory.
3. Sign and timestamp each staged executable with Jsign.
4. Verify each signature with `osslsigncode`.
5. Package the staged files with WiX, without rebuilding them.
6. Extract the MSI and compare all three executable hashes with the staged originals.
7. Sign, timestamp, and verify the MSI.
8. Publish the local artifacts after every selected architecture succeeds.

`--skip-sign` omits signature creation and verification.
Compilation, packaging, and the MSI content checks still run.

WiX ICE validation requires Windows and is disabled during Wine packaging.
Validate the completed installers on Windows before releasing them.

## Validate on Windows

Copy the artifacts to the Windows test machine.
For a signed build, inspect the standalone signatures:

```powershell
Get-ChildItem .\signed\*.exe, .\signed\*.msi |
    Get-AuthenticodeSignature |
    Format-Table Status, Path
```

Each signed file should report `Valid`.
An unsigned test build should report `NotSigned`.

Install the MSI and inspect its installed executables:

```powershell
$p = Start-Process msiexec.exe -Wait -PassThru -ArgumentList `
    '/i playit-windows-x86_64.msi /qn /norestart /l*v install.log'
$p.ExitCode
Get-AuthenticodeSignature 'C:\Program Files\playit_gg\bin\*.exe' |
    Format-Table Status, Path
& 'C:\Program Files\playit_gg\bin\playit.exe' version
& 'C:\Program Files\playit_gg\bin\playit.exe' status
```

Exit code `0` means installation succeeded without requiring a reboot.
The 32-bit installer uses `C:\Program Files (x86)\playit_gg` on 64-bit Windows.
Test service start/stop, the tray menu, portable mode, and live TCP/UDP forwarding.
Also test upgrading the signed public installer while its tray and CLI are running.

## Existing signing script

`build-scripts/windows-sign.sh` signs files downloaded from a release.
It cannot sign executables already embedded in an MSI.
Use `windows-build.sh` for releases that require signed programs inside the installer.
