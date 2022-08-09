
#[cfg(target_os = "windows")]
fn main() {
    embed_resource::compile("assets/windows.rc");
}

#[cfg(not(target_os = "windows"))]
fn main() {
}