# The playit program

* Latest Release: 1.1.X
* Offical Website: https://playit.gg
* Offical Downloads: https://playit.gg/download
* Releases: https://github.com/playit-cloud/playit-agent/releases

---

** Non deprecated releases of the playit program:
 `1.2.X`

---

## Installation

Download latest release for your platform from https://playit.gg/download and run the installer or binary.

### Installing on Windows

Alternatively, you can install via winget (Windows package manager):

```sh
winget install DevelopedMethods.playit
```

The installer sets up:

* **The `playit.gg` Windows service** (`playitd`). It starts with Windows, keeps your tunnels online, and retries on its own when the network is unavailable.
* **The tray icon**. It starts when you sign in and shows whether playit is online. Right-click it to open playit, start or stop the service, open the logs, or turn off "Launch at Login".
* **The Playit.gg shortcut**. It opens playit and brings the tray icon back if you closed it.

Logs are in `%ProgramData%\playit_gg\logs`. The standalone `playit-windows-x86_64.exe` runs playit in its own window without installing the service.

### Installing on macOS

Download `playit-<version>.pkg` (Apple Silicon and Intel) and open it. The installer sets up:

* **playit.app** in Applications. It lives in the menu bar and shows whether playit is online. Click the icon to open playit, start or stop the service, open the logs, or turn off "Open at Login".
* **The background service** (`playitd`). It runs as a per-user LaunchAgent, starts when you log in, keeps your tunnels online, and restarts if it crashes.
* **The `playit` command** in `/usr/local/bin`.

After installing, playit opens in Terminal with a link to finish setup. Other users on the Mac get the service the first time they open playit.app.

Logs are in `~/Library/Logs/playit`. Your agent's secret is in `~/Library/Application Support/playit_gg/playit.toml`. To remove playit, choose "Uninstall playit…" from the menu bar icon, or run `sudo /Applications/playit.app/Contents/Resources/uninstall.sh`. Uninstalling keeps the secret, so reinstalling reconnects the same agent.

To run playit without installing it, download `playit-macos-universal.tar.gz`, keep `playit` and `playitd` in the same folder, and run `./playit`. It installs the background service for your user on first run; `playit uninstall` removes it.

To build both from source, run `build-scripts/package-macos.sh` on a Mac.

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

**Note**
Please only use the playit program if you downloaded it from an offical source or are compiling and running from source.

### Docker

The container runs the daemon in the foreground and writes plain logs to stdout.
It requires no terminal or TUI.
Generate an agent secret key [here](https://playit.gg/account/setup/wizard/new-account/docker/docker-name).

On Linux, use host networking when the game server runs on the Docker host:

```sh
docker run -d --name playit --restart unless-stopped --network host \
  -e SECRET_KEY=YOUR_AGENT_SECRET ghcr.io/playit-cloud/playit-agent:latest
docker logs -f playit
docker stop playit
```

Alternatively, put the raw secret key in a file and mount it read-only:

```sh
docker run -d --name playit --restart unless-stopped --network host \
  --mount type=bind,src=/absolute/path/playit-secret,dst=/run/secrets/playit,readonly \
  -e SECRET_KEY_FILE=/run/secrets/playit ghcr.io/playit-cloud/playit-agent:latest
```

Set either `SECRET_KEY` or `SECRET_KEY_FILE`.
The first positional argument also accepts a secret key for compatibility.
The secret file contains only the key, without TOML syntax.

With bridge networking, `127.0.0.1` refers to the container itself.
Set tunnel origins to an address reachable from the container.
To share a game container's loopback interface, use `--network container:GAME_CONTAINER` instead of `--network host`.
No inbound port publishing is required for the agent.

Pass daemon options after the image name, such as `--socket-path /tmp/playit.sock`.
Use `-e PLAYIT_LOG=debug` for more detailed logs.
Run `docker run --rm ghcr.io/playit-cloud/playit-agent:latest --help` to list daemon options.

Build and test the current source with:

```sh
docker build -t playit-agent:docker-test .
docker build --target test .
./docker/test.sh playit-agent:docker-test
```

## Building / Running Locally

Requires Rust: https://rustup.rs

```sh
# Clone the repository
git clone https://github.com/playit-cloud/playit-agent.git
cd playit-agent

# Build and run the release version
cargo run --release
```
