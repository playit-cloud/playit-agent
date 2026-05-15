cargo build --release --all --target=x86_64-pc-windows-msvc
cargo wix --target x86_64-pc-windows-msvc --package playit-cli --nocapture --output=target/wix/x86_64-pc-windows-msvc.msi
