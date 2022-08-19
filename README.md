# The playit program

* Latest Release: 0.9.3
* Offical Website: https://playit.gg
* Offical Downloads: https://playit.gg/download
* Releases: https://github.com/playit-cloud/playit-agent/releases

Installing on ubuntu or debian

```
curl -SsL https://playit-cloud.github.io/ppa/key.gpg | sudo apt-key add -
sudo curl -SsL -o /etc/apt/sources.list.d/playit-cloud.list https://playit-cloud.github.io/ppa/playit-cloud.list
sudo apt update
sudo apt install playit
```

**Note**
Please only use the playit program if you downloaded if from an offical source or are compiling and running from source.

## Building / Running Locally

Requires Rust: https://rustup.rs
Run using `cargo run --release --bin=agent`

