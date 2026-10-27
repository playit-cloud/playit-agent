fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winres::WindowsResource::new();
    println!("cargo:rerun-if-env-changed=WINRES_TOOLKIT_PATH");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=../playit-cli/wix/Product.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    if let Ok(path) = std::env::var("WINRES_TOOLKIT_PATH") {
        resource.set_toolkit_path(&path);
    }
    resource
        .set_icon("../playit-cli/wix/Product.ico")
        .set_manifest(include_str!("app.manifest"))
        .compile()
        .unwrap();
}
