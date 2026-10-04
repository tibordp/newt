//! Panic reports, written to the log directory. One process hosts every
//! window, and a panic in a command handler (which runs inside the
//! webview's callback) ends all of them; the report is what's left to
//! look at. A panic in a background task ends only that task, and is
//! reported the same way.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Reports kept from earlier runs, besides this run's.
const KEEP: usize = 10;
/// A run's report stops growing here, so a panic that repeats can't fill
/// the disk.
const MAX_REPORT: u64 = 1 << 20;

/// Report every panic in `dir` (`panic-<start time>-<pid>.txt`, one file a
/// run), then as Rust would.
pub fn install(dir: PathBuf) {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let file = dir.join(format!("panic-{started}-{}.txt", std::process::id()));
    prune(&dir);
    let report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Err(e) = write(&dir, &file, info) {
            eprintln!("could not write a panic report to {}: {e}", file.display());
        }
        report(info);
    }));
}

fn write(dir: &Path, file: &Path, info: &std::panic::PanicHookInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if std::fs::metadata(file).is_ok_and(|m| m.len() > MAX_REPORT) {
        return Ok(());
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?;
    let thread = std::thread::current();
    writeln!(
        out,
        "Newt {} ({}, {}) panicked on thread {}:\n{info}\n\n{}\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("NEWT_GIT_REVISION").unwrap_or("unknown revision"),
        std::env::consts::OS,
        thread.name().unwrap_or("<unnamed>"),
        std::backtrace::Backtrace::force_capture(),
    )
}

/// Delete all but the newest `KEEP` reports.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("panic-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    reports.sort();
    let excess = reports.len().saturating_sub(KEEP);
    for (_, path) in reports.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_reports_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..KEEP + 3 {
            let path = dir.path().join(format!("panic-{i}.txt"));
            std::fs::write(&path, "x").unwrap();
            let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i as u64);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        std::fs::write(dir.path().join("other.log"), "x").unwrap();
        prune(dir.path());
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP + 1);
        assert!(left.contains(&"other.log".to_string()));
        assert!(!left.contains(&"panic-0.txt".to_string()));
    }
}
