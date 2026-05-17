//! Cross-platform agent build + layout.
//!
//! Builds `newt-agent` per target triple and lays it out as
//! `agents/<triple>/newt-agent[.exe]` — the tree the host resolver
//! (`TauriAgentResolver`) loads from the cwd-relative `agents/` dir.
//! `cargo xtask agents`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};

const LINUX: &[&str] = &["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"];
const DARWIN: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin"];
const WINDOWS: &[&str] = &["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"];

/// Agent file name for a triple. Mirrors
/// `newt_common::agent_resolver::agent_file_name` — kept in sync by hand
/// so this build tool doesn't have to compile the whole common crate.
fn agent_file_name(triple: &str) -> &'static str {
    if triple.contains("windows") {
        "newt-agent.exe"
    } else {
        "newt-agent"
    }
}

/// `(buildable, unbuildable)` for the current host.
///
/// linux-musl goes through `cargo zigbuild` (zig as cross-linker — works
/// from any host). darwin and windows-msvc use plain `cargo build`, which
/// can only target their own OS family without a foreign toolchain — so a
/// Windows host can produce Windows + Linux agents but not darwin, etc.
fn host_targets() -> (Vec<&'static str>, Vec<&'static str>) {
    let mut buildable: Vec<&str> = Vec::new();
    let mut unbuildable: Vec<&str> = Vec::new();

    if cfg!(target_os = "macos") {
        buildable.extend(DARWIN);
        buildable.extend(LINUX);
        unbuildable.extend(WINDOWS);
    } else if cfg!(target_os = "linux") {
        buildable.extend(LINUX);
        unbuildable.extend(DARWIN);
        unbuildable.extend(WINDOWS);
    } else if cfg!(target_os = "windows") {
        buildable.extend(WINDOWS);
        buildable.extend(LINUX);
        unbuildable.extend(DARWIN);
    }
    (buildable, unbuildable)
}

fn group(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "linux" => Some(LINUX),
        "darwin" => Some(DARWIN),
        "windows" => Some(WINDOWS),
        _ => None,
    }
}

fn cargo_subcommand(triple: &str) -> &'static str {
    if triple.ends_with("-unknown-linux-musl") {
        "zigbuild"
    } else {
        "build"
    }
}

fn build_one(triple: &str) -> Result<(), String> {
    let name = agent_file_name(triple);
    let target_dir = PathBuf::from("target-agents").join(triple);
    let sub = cargo_subcommand(triple);

    // Ensure the std component for the target is present. Idempotent;
    // ignored if rustup isn't on PATH (the build below then surfaces the
    // real error).
    let _ = Command::new("rustup")
        .args(["target", "add", triple])
        .status();

    println!("→ cargo {sub} --release --target {triple} -p newt-agent");
    let status = Command::new("cargo")
        .args([sub, "--release", "--target", triple, "-p", "newt-agent"])
        // Per-triple cache dir keeps each target isolated from the others
        // and from the main dev build in ./target.
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .map_err(|e| format!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        let mut msg = format!("cargo {sub} failed for {triple}");
        if sub == "zigbuild" {
            msg.push_str(" (linux agents need: cargo install cargo-zigbuild + zig)");
        }
        return Err(msg);
    }

    let src = target_dir.join(triple).join("release").join(name);
    let dest_dir = PathBuf::from("agents").join(triple);
    let dest = dest_dir.join(name);
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir {}: {e}", dest_dir.display()))?;
    std::fs::copy(&src, &dest)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dest.display()))?;
    write_sha256(&dest)?;
    println!("  {}", dest.display());
    Ok(())
}

/// `<sha256hex>  <path>\n` — same shape as `sha256sum` / `shasum -a 256`,
/// so the layout is byte-identical to the Makefile's.
fn write_sha256(file: &Path) -> Result<(), String> {
    let bytes = std::fs::read(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let sidecar = file.with_file_name(format!(
        "{}.sha256",
        file.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&sidecar, format!("{hex}  {}\n", file.display()))
        .map_err(|e| format!("write {}: {e}", sidecar.display()))
}

fn clean() -> Result<(), String> {
    for dir in ["agents", "target-agents"] {
        if Path::new(dir).exists() {
            std::fs::remove_dir_all(dir).map_err(|e| format!("rm -rf {dir}: {e}"))?;
        }
    }
    println!("removed agents/ and target-agents/");
    Ok(())
}

fn help() {
    let (buildable, unbuildable) = host_targets();
    let host = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unsupported"
    };
    println!("Host:   {host}");
    println!("Build:  {}", buildable.join(" "));
    if !unbuildable.is_empty() {
        println!(
            "Skip:   {} (cannot cross-compile from this host)",
            unbuildable.join(" ")
        );
    }
    println!();
    println!("  cargo xtask agents [all|linux|darwin|windows|<triple>]...");
    println!("      build agents (default: every triple this host can produce)");
    println!("  cargo xtask clean");
    println!("      remove agents/ and target-agents/");
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = args
        .split_first()
        .map(|(c, r)| (c.as_str(), r))
        .unwrap_or(("help", &[]));

    match cmd {
        "clean" => clean(),
        "agents" => {
            let (buildable, unbuildable) = host_targets();

            // No filter → everything this host can build. Filters are
            // group names or exact triples, intersected with `buildable`.
            let selected: Vec<&str> = if rest.is_empty() || rest == ["all"] {
                buildable.clone()
            } else {
                let mut out: Vec<&str> = Vec::new();
                for arg in rest {
                    let triples: Vec<&str> = match group(arg) {
                        Some(g) => g.to_vec(),
                        None => vec![arg.as_str()],
                    };
                    for t in triples {
                        if let Some(b) = buildable.iter().find(|b| **b == t) {
                            if !out.contains(b) {
                                out.push(b);
                            }
                        } else {
                            eprintln!("skip {t}: not buildable from this host");
                        }
                    }
                }
                out
            };

            if selected.is_empty() {
                return Err("nothing to build for this host".into());
            }
            if !unbuildable.is_empty() {
                println!(
                    "(skipping {} — cannot cross-compile from this host)",
                    unbuildable.join(" ")
                );
            }
            for triple in selected {
                build_one(triple)?;
            }
            Ok(())
        }
        "help" | "-h" | "--help" => {
            help();
            Ok(())
        }
        other => Err(format!("unknown command: {other} (try `cargo xtask help`)")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
