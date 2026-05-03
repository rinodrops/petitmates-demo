fn main() {
    // Embed Windows resources only when targeting Windows.
    // Runs on the host at compile time via `cargo build`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = std::path::Path::new(&manifest_dir).join("assets");

    // Rerun if the source icon files change.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", base.join("appicon.ico").display());
    println!("cargo:rerun-if-changed={}", base.join("trayicon.ico").display());
    println!("cargo:rerun-if-changed={}", base.join("trayicon-white.ico").display());

    // Copy ico files to /tmp to avoid windres issues with spaces in paths.
    std::fs::copy(base.join("appicon.ico"),        "/tmp/pm_icon.ico").unwrap();
    std::fs::copy(base.join("trayicon.ico"),        "/tmp/pm_trayicon.ico").unwrap();
    std::fs::copy(base.join("trayicon-white.ico"),  "/tmp/pm_trayicon_white.ico").unwrap();

    // RC source:
    //   ID 1 = exe / window-class icon (Explorer, title bar)
    //   ID 2 = system tray icon (monochrome silhouette)
    let rc_src = "#pragma code_page(65001)\n\
                  1 ICON \"/tmp/pm_icon.ico\"\n\
                  2 ICON \"/tmp/pm_trayicon.ico\"\n\
                  3 ICON \"/tmp/pm_trayicon_white.ico\"\n";
    std::fs::write("/tmp/pm_resource.rc", rc_src).unwrap();

    // Run windres → COFF object file.
    // Using --output-format=coff produces a .o that can be linked directly.
    let windres = if cfg!(target_os = "windows") { "windres" } else { "x86_64-w64-mingw32-windres" };
    let status = std::process::Command::new(windres)
        .args(["-i", "/tmp/pm_resource.rc", "-o", "/tmp/pm_resource.o", "--output-format=coff"])
        .status()
        .expect("windres not found — install mingw-w64");
    assert!(status.success(), "windres failed");

    // Link the resource object directly (not as a static lib).
    // cargo:rustc-link-lib=static with GNU ld silently drops unreferenced .rsrc;
    // passing the .o via rustc-link-arg forces unconditional inclusion.
    println!("cargo:rustc-link-arg=/tmp/pm_resource.o");
}
