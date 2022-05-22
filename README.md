# The playit program

* Latest Release: 0.8.1-beta
* Offical Website: https://playit.gg
* Offical Downloads: https://playit.gg/download

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

