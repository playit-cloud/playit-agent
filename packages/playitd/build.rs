#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("../playit-cli/wix/Product.ico");
    res.compile().unwrap();
}

#[cfg(not(windows))]
fn main() {}
