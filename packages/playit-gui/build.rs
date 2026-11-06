// Embeds the playit icon on Windows. winres gives it resource ID 1, which is the ID GPUI
// loads for the window icon, so the same icon shows on the exe and the window.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // GPUI depends on psm, which assembles its GNU-syntax source when cross-compiling from a
    // non-Windows host. That object lacks the SafeSEH marker, so 32-bit links fail under the
    // /SAFESEH that rustc passes.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86")
        && !std::env::var("HOST").unwrap_or_default().contains("windows")
    {
        println!("cargo:rustc-link-arg-bins=/SAFESEH:NO");
    }
    let mut resource = winres::WindowsResource::new();
    println!("cargo:rerun-if-env-changed=WINRES_TOOLKIT_PATH");
    println!("cargo:rerun-if-changed=../playit-cli/wix/Product.ico");
    if let Ok(path) = std::env::var("WINRES_TOOLKIT_PATH") {
        resource.set_toolkit_path(&path);
    }
    resource
        .set_icon("../playit-cli/wix/Product.ico")
        .compile()
        .unwrap();
}
