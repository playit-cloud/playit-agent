#!/bin/bash
OS_DIR="macos"
OUT_DIR="output"
cargo build --release --target-dir="$OS_DIR/$OUT_DIR"

cp $OS_DIR/$OUT_DIR/release/playit-cli /usr/local/bin
rm -rf $OS_DIR/$OUT_DIR
