use super::*;

// --- Run command ---

pub(super) async fn execute_run_command(
    reporter: &mut ProgressReporter,
    command: &str,
    working_dir: Option<&crate::vfs::path::Path>,
    shell_integration: Option<&crate::shell_control::ShellIntegration>,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    reporter.send_prepared(0, 0);
    reporter.maybe_send_progress(0, 0, command);

    let mut child = {
        let (shell, shell_args) = crate::shell::run_via_shell(command);
        let mut cmd = tokio::process::Command::new(shell);
        cmd.no_console_window();
        cmd.args(shell_args);
        if let Some(dir) = working_dir {
            // Native conversion happens here — the executor runs where
            // the FS is (the agent in a remote session). `launch_cwd`
            // (not `to_native`) so cmd.exe accepts a local directory.
            cmd.current_dir(dir.launch_cwd());
        }
        if let Some(si) = shell_integration {
            cmd.envs(si.spawn_env(None));
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        cmd.spawn()
            .map_err(|e| crate::Error::custom(format!("failed to spawn command: {}", e)))?
    };

    let status = tokio::select! {
        status = child.wait() => {
            status.map_err(|e| crate::Error::custom(format!("failed to wait for command: {}", e)))?
        }
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(crate::Error::custom("cancelled".to_string()));
        }
    };

    if status.success() {
        Ok(())
    } else {
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        Err(crate::Error::custom(format!(
            "command exited with code {}",
            code
        )))
    }
}
