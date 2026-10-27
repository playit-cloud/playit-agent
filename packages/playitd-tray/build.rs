#[cfg(windows)]
fn main() {
    winres::WindowsResource::new()
        .set_icon("../playit-cli/wix/Product.ico")
        .set_manifest(include_str!("app.manifest"))
        .compile()
        .unwrap();
}

#[cfg(not(windows))]
fn main() {}
