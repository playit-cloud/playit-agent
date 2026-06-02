# The playit program

* Latest Release: 1.0.X
* Offical Website: https://playit.gg
* Offical Downloads: https://playit.gg/download
* Releases: https://github.com/playit-cloud/playit-agent/releases

---

** Non deprecated releases of the playit program:
 `0.17.1` and `1.0.X`

---

## Installation

Download latest release for your platform from https://playit.gg/download and run the installer or binary.

### Installing on Windows

Alternatively, you can install via winget (Windows package manager):

```sh
winget install DevelopedMethods.playit
```

### Installing on Ubuntu or Debian

```sh
curl -SsL https://playit-cloud.github.io/ppa/key.gpg | gpg --dearmor | sudo tee /etc/apt/trusted.gpg.d/playit.gpg >/dev/null
echo "deb [signed-by=/etc/apt/trusted.gpg.d/playit.gpg] https://playit-cloud.github.io/ppa/data ./" | sudo tee /etc/apt/sources.list.d/playit-cloud.list
sudo apt update
sudo apt install playit
```

Getting a warning in apt about playit's repo? Run these commands

```sh
sudo apt-key del '16AC CC32 BD41 5DCC 6F00  D548 DA6C D75E C283 9680'
sudo rm /etc/apt/sources.list.d/playit-cloud.list
sudo apt update

curl -SsL https://playit-cloud.github.io/ppa/key.gpg | gpg --dearmor | sudo tee /etc/apt/trusted.gpg.d/playit.gpg >/dev/null
echo "deb [signed-by=/etc/apt/trusted.gpg.d/playit.gpg] https://playit-cloud.github.io/ppa/data ./" | sudo tee /etc/apt/sources.list.d/playit-cloud.list
sudo apt update
```

### Installing on Fedora

Download the RPM for your architecture from the latest GitHub release, then install it with `dnf`:

```sh
sudo dnf install ./playit_x86_64.rpm
```

Available RPM architectures are `x86_64`, `aarch64`, `armv7hl`, and `i686`.

### Installing on Arch Linux

Install the binary AUR package:

```sh
yay -S playit-bin
```

The generated `playit-bin` AUR package files are also attached to GitHub releases as `playit-bin-PKGBUILD.tar.gz` for maintainers and manual publishing.

**Note**
Please only use the playit program if you downloaded it from an offical source or are compiling and running from source.

### Docker

```sh
docker run --rm -it --net=host -e SECRET_KEY=<secret key> ghcr.io/playit-cloud/playit-agent:latest
```

> [!NOTE]
> Secret key can be generated [here](https://playit.gg/account/setup/wizard/new-account/docker/docker-name).

## Building / Running Locally

Requires Rust: https://rustup.rs

```sh
# Clone the repository
git clone https://github.com/playit-cloud/playit-agent.git
cd playit-agent

# Build and run the release version
cargo run --release
```
