//! `cfg(freedesktop)`: desktops reached through the D-Bus session bus
//! (Linux, BSD). macOS and Windows use their native services instead.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(freedesktop)");
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if family.split(',').any(|f| f == "unix") && os != "macos" {
        println!("cargo::rustc-cfg=freedesktop");
    }
}
