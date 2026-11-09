//! Windows: link the app icon in as icon resource 1, which is where GPUI's Windows backend
//! loads the window's (and so the taskbar's) icon from. Elsewhere there is nothing to do: on
//! macOS the icon comes from the bundle.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows_icon();
}

#[cfg(windows)]
fn windows_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("../../packaging/assets/Midna.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    if !ico.is_file() {
        println!("cargo:warning=no {} (packaging/make-icon.py writes it): built without an icon", ico.display());
        return;
    }
    let rc = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("midna.rc");
    // rc.exe takes forward slashes, which need no escaping in its strings
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"))).unwrap();
    // A missing resource compiler leaves the icon out (a warning), not the build.
    if let Err(e) = embed_resource::compile(&rc, embed_resource::NONE).manifest_required() {
        println!("cargo:warning=built without an icon: {e}");
    }
}
