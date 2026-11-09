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
* **The tray icon**. It starts when you sign in and shows whether playit is online. Click it to open the playit window, or right-click it to start or stop the service, open the logs, or turn off "Launch at Login".
* **The Playit.gg shortcut**. It opens the playit window, starts the service if it isn't running, and brings the tray icon back if you closed it.
* **The `playit` command**. Type `playit` in a terminal for the command-line version.

Logs are in `%ProgramData%\playit_gg\logs`. The standalone `playit-windows-x86_64.exe` runs playit in its own window without installing the service.

### Installing on macOS

Download `playit-<version>.pkg` (Apple Silicon and Intel) and open it. The installer sets up:

* **playit.app** in Applications. Opening it shows the playit window and starts the service if it isn't running. A menu bar icon shows whether playit is online; click it to open playit, start or stop the service, open the logs, or turn off "Open at Login".
* **The background service** (`playitd`). It runs as a per-user LaunchAgent, starts when you log in, keeps your tunnels online, and restarts if it crashes.
* **The `playit` command** in `/usr/local/bin`, the command-line version.

After installing, the playit window opens with a link to finish setup. Other users on the Mac get the service the first time they open playit.app.

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

### The playit window on Linux desktops

On 64-bit x86 and ARM, the `playit` package also installs the desktop window and adds it to your app launcher with its icon; open it from the launcher or run `playit-gui`. `playit` in a terminal is still the command-line version. Alpine's `playit-openrc` package is for headless systems such as routers and installs only the command-line version and service.

The window's libraries are recommended rather than required on Debian, Ubuntu and Fedora, so servers can leave them out with `apt install --no-install-recommends playit` (or `dnf install --setopt=install_weak_deps=False playit`). The window also needs a Vulkan driver, which desktop installs normally include.

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

HTTPS tunnels can have playit terminate TLS with a free Let's Encrypt certificate (set
"TLS Termination" to `letsencrypt` on the tunnel). Certificates are stored in the
`/var/lib/playit` volume; mount a named volume so they survive recreating the container
and aren't requested again:

```sh
docker run -d --name playit --restart unless-stopped --network host \
  -v playit-data:/var/lib/playit \
  -e SECRET_KEY=YOUR_AGENT_SECRET ghcr.io/playit-cloud/playit-agent:latest
```

Set `CERT_DIR` or pass `--cert-dir` to store them elsewhere.

Pass daemon options after the image name, such as `--socket-path /tmp/playit.sock`.
Use `-e PLAYIT_LOG=debug` for more detailed logs.
Run `docker run --rm ghcr.io/playit-cloud/playit-agent:latest --help` to list daemon options.

Build and test the current source with:

```sh
docker build -t playit-agent:docker-test .
docker build --target test .
./docker/test.sh playit-agent:docker-test
```

## HTTPS tunnels with TLS termination

Set "TLS Termination" to `letsencrypt` on an HTTPS tunnel to have playit serve HTTPS for you.
playit gets a certificate from Let's Encrypt using the HTTP challenge on the tunnel's HTTP
port, renews it in the window Let's Encrypt suggests (or when it has fewer than 30 days
left), and forwards decrypted requests to your
local server with `X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto` and `X-Forwarded-Host`
headers. WebSocket upgrades are passed through.

- `X-Forwarded-For` and `X-Real-IP` hold the visitor's IP address, as seen by playit. Any
  forwarding headers the visitor sends are replaced, so your server can trust them.
- Requests go to the HTTP port. Set the HTTP port to 0 to send them to the HTTPS port
  instead; the local server's certificate is not checked, so a self-signed one works.
  If both ports are set, HTTP is used.
- Plain HTTP requests to the tunnel are redirected to HTTPS.
- The terminal UI and the playit window show the certificate's progress. HTTPS connections
  are refused until the certificate is ready.
- Before asking Let's Encrypt, playit checks that it can reach the tunnel through its public
  domain. If your network can't reach its own public address, set
  `PLAYIT_ACME_SKIP_SELF_CHECK=1` to skip this check.
- If Let's Encrypt rate-limits a request, playit waits until the retry time Let's Encrypt
  gives before trying again.
- Set `PLAYIT_ACME_DIRECTORY` to use another ACME server, such as Let's Encrypt staging
  (`https://acme-staging-v02.api.letsencrypt.org/directory`) for testing.
  Set `PLAYIT_ACME_ROOT_CERT` to a PEM file to trust a private ACME server's certificate.

### Where certificates are stored

Certificates are stored in a `certs` folder beside the secret file:

| Platform | Folder |
| --- | --- |
| Linux (deb, rpm or apk package, systemd or OpenRC service) | `/etc/playit/certs` |
| macOS | `~/Library/Application Support/playit_gg/certs` |
| Windows (installed service) | `C:\ProgramData\playit_gg\certs` |
| Windows (portable, no service installed) | `%LOCALAPPDATA%\playit_gg\certs` |
| Docker | `/var/lib/playit/certs`, inside the `/var/lib/playit` volume |
| `playitd` run by hand | `certs` beside the file given to `--secret-path` |

Pass `--cert-dir` to `playitd` to use another folder. In Docker, you can set `CERT_DIR` instead.

The folder holds the Let's Encrypt account key in `acme-account.json`, and one folder per
domain with `fullchain.pem` (the certificate chain) and `privkey.pem` (its private key).

Only the user running playit can read these files. On Linux and macOS, folders are
created with mode `700` and files with mode `600`. On Windows, access is limited to that
user, SYSTEM and Administrators. On Linux the service runs as the `playit` user. If you
choose an existing folder with `--cert-dir`, playit leaves that folder's permissions
unchanged, but restricts the files and folders it creates inside it.

Deleting a domain's folder makes playit request a new certificate the next time it starts.
Let's Encrypt allows 5 certificates for the same domain each week.

## Building / Running Locally

Requires Rust: https://rustup.rs

```sh
# Clone the repository
git clone https://github.com/playit-cloud/playit-agent.git
cd playit-agent

# Build and run the release version
cargo run --release
```
