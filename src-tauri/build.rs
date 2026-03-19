use std::process::Command;

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

    // Expose the target triple so find_local_agent_binary can locate
    // the correct agent binary under agents/<triple>/newt-agent.
    println!(
        "cargo:rustc-env=NEWT_TARGET_TRIPLE={}",
        std::env::var("TARGET").unwrap()
    );

    // Pass through optional system agent directory for distro packages.
    if let Ok(dir) = std::env::var("NEWT_SYSTEM_AGENT_DIR") {
        println!("cargo:rustc-env=NEWT_SYSTEM_AGENT_DIR={}", dir);
    }

    // Git revision (short hash + dirty flag). Gracefully falls back for
    // builds outside a git repo or without git installed.
    let git_rev = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let git_dirty = Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .status()
        .ok()
        .map(|s| !s.success())
        .unwrap_or(false);

    if let Some(rev) = git_rev {
        let suffix = if git_dirty { "+" } else { "" };
        println!("cargo:rustc-env=NEWT_GIT_REVISION={}{}", rev, suffix);
    }

    // Build date (UTC, YYYY-MM-DD)
    let now = time::OffsetDateTime::now_utc();
    println!(
        "cargo:rustc-env=NEWT_BUILD_DATE={:04}-{:02}-{:02}",
        now.year(),
        now.month() as u8,
        now.day()
    );

    tauri_build::build()
}
