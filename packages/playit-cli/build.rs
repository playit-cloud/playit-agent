fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winres::WindowsResource::new();
    println!("cargo:rerun-if-env-changed=WINRES_TOOLKIT_PATH");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=wix/Product.ico");
    if let Ok(path) = std::env::var("WINRES_TOOLKIT_PATH") {
        res.set_toolkit_path(&path);
    }
    res.set_icon("wix/Product.ico");
    res.compile().unwrap();
}
