# The playit program

* Latest Release: 0.16.X
* Offical Website: https://playit.gg
* Offical Downloads: https://playit.gg/download
* Releases: https://github.com/playit-cloud/playit-agent/releases

---

** Non deprecated releases of the playit program:
`0.15.26` and `0.16.2`

---

Installing on ubuntu or debian

```
curl -SsL https://playit-cloud.github.io/ppa/key.gpg | gpg --dearmor | sudo tee /etc/apt/trusted.gpg.d/playit.gpg >/dev/null
echo "deb [signed-by=/etc/apt/trusted.gpg.d/playit.gpg] https://playit-cloud.github.io/ppa/data ./" | sudo tee /etc/apt/sources.list.d/playit-cloud.list
sudo apt update
sudo apt install playit
```

Getting a warning in apt about playit's repo? Run these commands

```
sudo apt-key del '16AC CC32 BD41 5DCC 6F00  D548 DA6C D75E C283 9680'
sudo rm /etc/apt/sources.list.d/playit-cloud.list
sudo apt update

curl -SsL https://playit-cloud.github.io/ppa/key.gpg | gpg --dearmor | sudo tee /etc/apt/trusted.gpg.d/playit.gpg >/dev/null
echo "deb [signed-by=/etc/apt/trusted.gpg.d/playit.gpg] https://playit-cloud.github.io/ppa/data ./" | sudo tee /etc/apt/sources.list.d/playit-cloud.list
sudo apt update
```

**Note**
Please only use the playit program if you downloaded if from an offical source or are compiling and running from source.

## Building / Running Locally

> [!NOTE]
> For Mac, you can run `chmod +x ./macos/setup.sh && sudo ./macos/setup.sh` 
> And then to use binary: `playit-cli --help`

Requires Rust: https://rustup.rs
Run using `cargo run --release`

## Docker

```
docker run --rm -it --net=host -e SECRET_KEY=<secret key> ghcr.io/playit-cloud/playit-agent:latest
```

> [!NOTE]
> Secret key can be generated [here](https://playit.gg/account/agents/new-docker).
