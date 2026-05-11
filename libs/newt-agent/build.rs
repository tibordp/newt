use std::process::Command;

fn main() {
    // Expose the target triple — used by `--print-triple` and the long
    // version string.
    let target = std::env::var("TARGET").unwrap();
    println!("cargo:rustc-env=NEWT_TARGET_TRIPLE={}", target);

    // Git revision (short hash + dirty flag). Falls back gracefully for
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
    // Written to a file (rather than `cargo:rustc-env`) because the cargo
    // directive truncates at the first newline.
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap();
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
}
