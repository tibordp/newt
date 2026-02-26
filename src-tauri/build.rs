fn main() {
    // Ensure agent stub files exist so Tauri's resource validation passes
    // during dev builds. In CI/release builds, real binaries are placed here
    // before `tauri build` runs.
    let agent_targets = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    ];
    for target in &agent_targets {
        let dir = std::path::PathBuf::from(format!("../agents/{}", target));
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("newt-agent");
        if !path.exists() {
            std::fs::write(&path, b"").ok();
        }
    }

    tauri_build::build()
}
