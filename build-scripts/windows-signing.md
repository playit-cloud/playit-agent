# Signing the Windows release

This playbook signs every Windows file that playit ships: the three programs inside the MSI, the MSI itself, and the standalone downloads.

Windows Defender, SmartScreen and other antivirus products trust signed files more than unsigned ones. An unsigned `playitd.exe` runs as a service under the SYSTEM account, so antivirus software is more likely to block or quarantine it. The current CI signs only the outer MSI and the standalone exe (`windows-sign.sh`). The programs inside the MSI stay unsigned.

## Why the order matters

The MSI stores a copy of each program and records its size. A signature changes the file, so you must sign each program before WiX packages it. You cannot sign the programs inside an MSI after it is built.

After you sign a program, do not build it again. A new build replaces the signed file with an unsigned one.

The order of operations is:

1. Build the programs.
2. Sign the programs.
3. Verify the program signatures.
4. Package the MSI from the signed programs, without a new build.
5. Sign the MSI.
6. Verify the MSI signature and the signatures of the programs inside it.
7. Copy the signed files to their release names.
8. Publish the files.

## What gets signed

Each release has two architectures. You sign four files for each architecture, so eight signatures in total.

| Architecture | Rust target | Release suffix |
|---|---|---|
| 64-bit | `x86_64-pc-windows-msvc` | `x86_64` |
| 32-bit | `i686-pc-windows-msvc` | `x86` |

| Build output | Name inside the MSI | Standalone release asset |
|---|---|---|
| `playit-cli.exe` | `playit.exe` | `playit-windows-<suffix>.exe` |
| `playitd.exe` | `playitd.exe` | `playitd-windows-<suffix>.exe` |
| `playitd-tray.exe` | `playitd-tray.exe` | (none) |
| `target\wix\playit-windows-<suffix>.msi` | (none) | `playit-windows-<suffix>.msi` |

Note: releases before the Windows overhaul also contain `playitd-service.exe` and `playitd-windows-setup.exe`. These files no longer exist.

## Prerequisites

WiX Toolset 3 runs only on Windows. Do this procedure on one Windows machine that has the YubiKey connected.

Install these tools on the Windows machine:

* Rust (stable) with the targets `x86_64-pc-windows-msvc` and `i686-pc-windows-msvc`.
* Visual Studio Build Tools with the "Desktop development with C++" workload.
* WiX Toolset 3.14. Add its `bin` folder to `PATH`.
* cargo-wix (`cargo install cargo-wix`).
* Java 11 or later, to run `build-scripts\jsign-5.0.jar`.
* Yubico PIV Tool. jsign uses its `ykcs11` library to talk to the YubiKey.

You also need:

* The YubiKey with the code-signing key in its PIV slot.
* Its PIN.
* The certificate chain at `build-scripts\digicert.pem`.

If you do not have a Windows machine, read [Packaging in CI](#packaging-in-ci).

## Procedure

Run all commands in PowerShell, from the repository root.

### 1. Prepare the session

1. Check out the release tag:

   ```powershell
   git checkout v1.0.13
   git status
   ```

2. If `git status` shows changes, stop and clean the working tree.
3. Set the version and targets:

   ```powershell
   $Version = "1.0.13"
   $Targets = @{ "x86_64-pc-windows-msvc" = "x86_64"; "i686-pc-windows-msvc" = "x86" }
   ```

4. Read the YubiKey PIN into a variable. `Read-Host` keeps the PIN out of the command history.

   ```powershell
   $Pin = Read-Host "YubiKey PIN"
   ```

5. Define the signing function:

   ```powershell
   function Sign-Files([string[]] $Files) {
       java -jar build-scripts\jsign-5.0.jar `
           --storetype YUBIKEY --keystore PIV --storepass $Pin `
           --certfile build-scripts\digicert.pem `
           --alg SHA-256 `
           --tsaurl http://timestamp.digicert.com --tsmode RFC3161 `
           --name "playit" --url "https://playit.gg" `
           --replace `
           $Files
       if ($LASTEXITCODE -ne 0) { throw "jsign failed for: $Files" }
   }
   ```

Notes on the options:

* `--name` sets the program name that Windows shows in the UAC prompt for the MSI.
* `--replace` replaces an earlier signature, so a second run does not add a nested signature.
* The timestamp keeps the signatures valid after the certificate expires.

CAUTION: Type the PIN correctly. The YubiKey locks the PIV key after three incorrect PIN attempts.

### 2. Build the programs

1. Build both architectures:

   ```powershell
   foreach ($Target in $Targets.Keys) {
       cargo build --release --target $Target --package playit-cli --package playitd --package playitd-tray
       if ($LASTEXITCODE -ne 0) { throw "build failed for $Target" }
   }
   ```

### 3. Sign the programs

Sign the files where cargo built them. `cargo wix` packages the files from these folders.

1. Sign the three programs of each architecture:

   ```powershell
   foreach ($Target in $Targets.Keys) {
       $Bin = "target\$Target\release"
       Sign-Files @("$Bin\playit-cli.exe", "$Bin\playitd.exe", "$Bin\playitd-tray.exe")
   }
   ```

### 4. Verify the program signatures

1. Show the signature status and timestamp of the six programs:

   ```powershell
   $Programs = foreach ($Target in $Targets.Keys) {
       Get-ChildItem "target\$Target\release\playit-cli.exe", "target\$Target\release\playitd.exe", "target\$Target\release\playitd-tray.exe"
   }
   $Programs | Get-AuthenticodeSignature |
       Format-Table Status, @{ n = "Timestamped"; e = { $null -ne $_.TimeStamperCertificate } }, Path
   ```

2. Make sure that each row shows `Valid` and `True`.
3. If a row is not `Valid`, sign that file again (step 3).
4. If a row is not timestamped, sign that file again. The timestamp server can fail for a short time.

### 5. Package the MSI from the signed programs

1. Package both architectures:

   ```powershell
   foreach ($Target in $Targets.Keys) {
       $Suffix = $Targets[$Target]
       cargo wix --no-build --nocapture --package playit-cli --target $Target `
           --output "target\wix\playit-windows-$Suffix.msi"
       if ($LASTEXITCODE -ne 0) { throw "packaging failed for $Target" }
   }
   ```

CAUTION: Always use `--no-build`. Without it, `cargo wix` runs `cargo build`, which can replace the signed programs with unsigned ones.

### 6. Sign the MSI

1. Sign both MSI files:

   ```powershell
   Sign-Files @("target\wix\playit-windows-x86_64.msi", "target\wix\playit-windows-x86.msi")
   ```

### 7. Verify the MSI and its contents

1. Verify the MSI signatures:

   ```powershell
   Get-AuthenticodeSignature target\wix\*.msi | Format-Table Status, Path
   ```

2. Extract each MSI with an administrative install. This copies the files to a folder and does not install playit.

   ```powershell
   foreach ($Suffix in $Targets.Values) {
       $Out = "$PWD\target\msi-check\$Suffix"
       Remove-Item -Recurse -Force $Out -ErrorAction SilentlyContinue
       Start-Process msiexec -Wait -ArgumentList "/a", "target\wix\playit-windows-$Suffix.msi", "/qn", "TARGETDIR=$Out"
   }
   ```

3. Verify the extracted programs:

   ```powershell
   Get-ChildItem -Recurse target\msi-check -Filter *.exe | Get-AuthenticodeSignature |
       Format-Table Status, Path
   ```

4. Make sure that you see six programs (three for each architecture) and that each shows `Valid`.
5. If a program inside an MSI is not signed, start again at step 2.

### 8. Copy the signed files to their release names

The CI already uploaded unsigned files with the plain names. Upload the signed files under both the plain and the `-signed` names. Then every Windows download is signed, and `download.sh` finds the names it expects.

Note: Renaming a signed file does not change its signature.

1. Copy the files:

   ```powershell
   $Assets = "target\release-assets"
   New-Item -ItemType Directory -Force $Assets | Out-Null
   foreach ($Target in $Targets.Keys) {
       $Suffix = $Targets[$Target]
       $Bin = "target\$Target\release"
       foreach ($Name in "playit-windows-$Suffix", "playit-windows-$Suffix-signed") {
           Copy-Item "target\wix\playit-windows-$Suffix.msi" "$Assets\$Name.msi"
           Copy-Item "$Bin\playit-cli.exe" "$Assets\$Name.exe"
       }
       foreach ($Name in "playitd-windows-$Suffix", "playitd-windows-$Suffix-signed") {
           Copy-Item "$Bin\playitd.exe" "$Assets\$Name.exe"
       }
   }
   ```

2. Record the hashes for the release notes:

   ```powershell
   Get-FileHash "$Assets\*" -Algorithm SHA256 | Format-Table Hash, Path
   ```

### 9. Publish

1. Upload the files to the GitHub release. `--clobber` replaces the unsigned CI files.

   ```powershell
   gh release upload "v$Version" (Get-ChildItem target\release-assets\*).FullName --clobber
   ```

2. On a Linux or macOS machine with `s3cmd`, copy the release to R2:

   ```bash
   bash build-scripts/upload-r2.sh 1.0.13
   ```

3. Update the winget manifest with the signed MSI files. `wingetcreate` reads the hash and the ProductCode from the files.

   ```powershell
   wingetcreate update DevelopedMethods.playit --version $Version --submit `
       --urls "https://github.com/playit-cloud/playit-agent/releases/download/v$Version/playit-windows-x86_64.msi" `
              "https://github.com/playit-cloud/playit-agent/releases/download/v$Version/playit-windows-x86.msi"
   ```

4. Clear the PIN from the session:

   ```powershell
   Remove-Variable Pin
   ```

Note: Each `cargo wix` run creates a new ProductCode. The winget manifest must use the values from the MSI files you published, not from the CI build.

### 10. Smoke test

1. On a clean Windows virtual machine, download `playit-windows-x86_64.msi` from the release.
2. Open the MSI.
3. Make sure that the UAC prompt shows "Verified publisher: Developed Methods LLC" and the program name "playit".
4. Finish the install.
5. Verify the installed programs:

   ```powershell
   Get-AuthenticodeSignature "C:\Program Files\playit_gg\bin\*.exe" | Format-Table Status, Path
   ```

6. Make sure that the `playit.gg` service is running and the tray icon shows.

## Packaging in CI

If you do not have a Windows machine, CI can do the build and the packaging. You sign between the two steps. The order of operations does not change.

This needs changes to `.github/workflows/build-release.yml` that are not made yet:

1. The release job builds the programs and uploads the three `.exe` files of each architecture as workflow artifacts. It does not build the MSI.
2. You download the artifacts and sign the programs with jsign on Linux or macOS. Use the same options as `Sign-Files`.
3. You upload the signed programs to the release.
4. A second job, started manually with `workflow_dispatch`, downloads the signed programs into `target\<target>\release`. It runs `cargo wix --no-build` and uploads the MSI files.
5. You download the MSI files and sign them.
6. You verify and publish the files as in steps 7 to 9.

On Linux, you can verify with these tools:

* Signatures: `osslsigncode verify -in <file>`.
* MSI contents: `msiextract -C <folder> <file.msi>` (from msitools), then verify each extracted `.exe`.

## Related scripts

* `windows-sign.sh` signs only the outer MSI and the standalone exe of a finished release. This playbook replaces it, because that order cannot sign the programs inside the MSI.
* `download.sh` does not list the `playitd-windows-*` assets yet. Add them so that `upload-r2.sh` copies them to R2.
