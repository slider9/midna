//! Windows: link the app icon in as icon resource 1, which is where GPUI's Windows backend
//! loads the window's (and so the taskbar's) icon from, and a version resource whose name is
//! what Windows shows for the program (Task Manager, Settings ▸ Apps ▸ Startup). Midna Dev
//! (built with `MIDNA_DEV_HOME`) gets the inverted icon and its own name. Elsewhere there is
//! nothing to do: on macOS the icon comes from the bundle.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows_resources();
}

#[cfg(windows)]
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-env-changed=MIDNA_DEV_HOME");
    println!("cargo:rerun-if-env-changed=MIDNA_BUILD_VERSION");
    let dev = std::env::var("MIDNA_DEV_HOME").is_ok_and(|h| !h.is_empty());
    let name = if dev { "Midna Dev" } else { "Midna" };
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("../../packaging/assets").join(if dev { "MidnaDev.ico" } else { "Midna.ico" });
    println!("cargo:rerun-if-changed={}", ico.display());
    let mut rc = String::new();
    if ico.is_file() {
        // rc.exe takes forward slashes, which need no escaping in its strings
        rc.push_str(&format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/")));
    } else {
        println!("cargo:warning=no {} (packaging/make-icon.py writes it): built without an icon", ico.display());
    }
    let version = std::env::var("MIDNA_BUILD_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    // FILEVERSION wants four numbers: 0.1.0-beta.46 is 0,1,0,46.
    let mut nums: Vec<u16> = version.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).take(4).collect();
    nums.resize(4, 0);
    let nums = nums.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    rc.push_str(&format!(
        r#"1 VERSIONINFO
FILEVERSION {nums}
PRODUCTVERSION {nums}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "{name}"
      VALUE "ProductName", "{name}"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "midna-app.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    ));
    let path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("midna.rc");
    std::fs::write(&path, rc).unwrap();
    // A missing resource compiler leaves the resources out (a warning), not the build.
    if let Err(e) = embed_resource::compile(&path, embed_resource::NONE).manifest_required() {
        println!("cargo:warning=built without an icon or version resource: {e}");
    }
}
