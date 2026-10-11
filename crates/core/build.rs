use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // ScreenCaptureKit's Swift bridge needs compatibility archives at link time.
    // Ask the selected Swift toolchain; this works with Xcode and Command Line Tools.
    let output = Command::new("xcrun")
        .args(["swiftc", "-print-target-info"])
        .output()
        .expect("Device audio capture requires the macOS Swift toolchain");
    assert!(
        output.status.success(),
        "Could not inspect the Swift toolchain"
    );
    let info: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("Swift target information must be JSON");
    for path in info["paths"]["runtimeLibraryPaths"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(path) = path.as_str() {
            println!("cargo:rustc-link-search=native={path}");
        }
    }
}
