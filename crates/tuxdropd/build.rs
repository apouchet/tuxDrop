//! `cfg(freedesktop)`: desktops reached through the D-Bus session bus
//! (Linux, BSD). macOS and Windows use their native services instead.
//!
//! On Windows (MSVC), embeds the application manifests of
//! `packaging/windows` in the programs.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(freedesktop)");
    println!("cargo::rerun-if-changed=build.rs");
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if family.split(',').any(|f| f == "unix") && os != "macos" {
        println!("cargo::rustc-cfg=freedesktop");
    }
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows");
        let manifest = |name: &str| {
            let path = dir.join(name);
            println!("cargo::rerun-if-changed={}", path.display());
            format!("/MANIFESTINPUT:{}", path.display())
        };
        let common = manifest("tuxdropd.manifest");
        for bin in ["tuxdropd", "tuxdrop-share"] {
            println!("cargo::rustc-link-arg-bin={bin}=/MANIFEST:EMBED");
            println!("cargo::rustc-link-arg-bin={bin}={common}");
        }
        let share = manifest("tuxdrop-share.manifest");
        println!("cargo::rustc-link-arg-bin=tuxdrop-share={share}");
    }
}
