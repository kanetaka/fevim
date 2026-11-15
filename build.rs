//! Embeds the application icon into the Windows executable.
//!
//! GPUI's Windows backend loads the window/taskbar icon from the executable's
//! icon resource with ID 1, which is what `winresource::WindowsResource::set_icon`
//! produces.

fn main() {
    println!("cargo:rerun-if-changed=asset/icon/fevim.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("asset/icon/fevim.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed application icon: {e}");
        }
    }
}
