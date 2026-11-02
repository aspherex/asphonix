//! Embeds the SONIX icon (the WebView app's `src-tauri/icons/icon.ico`) as resource 1, which GPUI uses for the
//! window, taskbar and Alt+Tab icon; Explorer shows it for the exe.
fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/sonix.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("res/app.rc", embed_resource::NONE).manifest_optional().expect("embed app icon");
    }
}
