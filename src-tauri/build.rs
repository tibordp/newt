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

    let git_rev_display = git_rev.map(|rev| {
        let suffix = if git_dirty { "+" } else { "" };
        format!("{}{}", rev, suffix)
    });
    if let Some(rev) = &git_rev_display {
        println!("cargo:rustc-env=NEWT_GIT_REVISION={}", rev);
    }

    // Long-form version string used by clap for both `-V` and `--version`.
    // Mirrors the About dialog so the two stay in sync. Written to a file
    // (rather than `cargo:rustc-env`) because the cargo directive truncates
    // at the first newline.
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let target = std::env::var("TARGET").unwrap();
    let mut long_version = format!("v{}", pkg_version);
    if let Some(rev) = &git_rev_display {
        long_version.push_str(&format!(" ({})", rev));
    }
    long_version.push_str(&format!("\n{}", target));
    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(
        std::path::PathBuf::from(&out_dir).join("long_version.txt"),
        long_version,
    )
    .unwrap();

    tauri_build::build()
}
