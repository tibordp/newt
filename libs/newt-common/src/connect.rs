//! Agent connection establishment over spawn-style transports (SSH, docker,
//! podman, kubectl, custom shell). Symmetric infrastructure: the host uses it
//! for full remote sessions; the agent will use the same paths to spawn
//! sub-agents for pane-scoped mounts (see DESIGN_AGENT_VFS_MOUNTS.md).

use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::Error;
use crate::agent_resolver::AgentResolver;
use crate::askpass::AskpassProvider;
use crate::proc::NoConsoleWindow;

const BOOTSTRAP_SCRIPT: &str = include_str!("../../../scripts/bootstrap.sh");

/// Sink for human-readable connection progress lines (spawn commands,
/// bootstrap negotiation, upload progress). The host feeds these into the
/// connection screen; a mount-scoped spawn feeds them into VFS mount
/// progress.
pub trait ConnectLog: Send + Sync {
    fn log(&self, line: String);
}

/// On Linux, arrange for the child to receive SIGTERM when the parent exits.
/// This ensures SSH/agent processes don't linger if Newt is killed.
/// On other platforms this is a no-op.
pub fn set_parent_death_signal(cmd: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_PDEATHSIG) is async-signal-safe and this is
        // the only thing we do in the pre_exec closure.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = cmd;
    }
}

// ---------------------------------------------------------------------------
// SpawnSpec
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum SpawnSpec {
    /// Run `transport_cmd` and append the sh-based `bootstrap.sh` script as its
    /// final argument. The bootstrap negotiates arch detection, agent caching,
    /// and upload-if-missing. Requires `sh` + a handful of coreutils on the
    /// target side; in exchange we get a hash-keyed agent cache.
    Bootstrap {
        transport_cmd: Vec<String>,
        /// Human-readable label, used in log lines and the connection log.
        label: String,
        /// Whether this transport supports interactive prompts via SSH_ASKPASS.
        /// `true` for `ssh` so passwords / passphrases can be forwarded;
        /// `false` for daemon-mediated transports (docker / kubectl etc.) where
        /// SSH_ASKPASS is a no-op.
        askpass: bool,
        /// `true` if the transport joins its trailing argv elements into a
        /// single shell command on the far side (this is what `ssh` does, and
        /// it requires us to shell-quote the bootstrap into one argv element).
        /// `false` for transports that `execvp` their args directly
        /// (`docker exec`, `podman exec`, `kubectl exec`, custom), where we
        /// must pass `sh`, `-c`, `<script>` as three separate argv elements.
        shell_join: bool,
    },
    /// Out-of-band copy: detect the target's architecture, `cp` the agent
    /// binary in, then exec it directly. No shell on the target side required.
    /// Re-uploads on every connect (no cache).
    DirectCopy(DirectCopyPlan),
    /// User-supplied shell command run locally. The bootstrap script is exposed
    /// via the `NEWT_BOOTSTRAP` env var; the user references it from inside
    /// their command (`ssh host "$NEWT_BOOTSTRAP"`, `bash -c "$NEWT_BOOTSTRAP"`,
    /// etc.). Gives the most control at the cost of needing the user to write
    /// the splice point themselves.
    CustomShell {
        command: String,
        label: String,
        /// If true, do not run the bootstrap handshake — assume the command
        /// produces a running agent on stdin/stdout itself. Default false.
        skip_bootstrap: bool,
    },
}

impl SpawnSpec {
    pub fn label(&self) -> &str {
        match self {
            SpawnSpec::Bootstrap { label, .. } => label,
            SpawnSpec::DirectCopy(p) => &p.label,
            SpawnSpec::CustomShell { label, .. } => label,
        }
    }
}

/// Recipe for a bootstrapless launch. Each command is a fully-resolved argv
/// except for the `{local}` / `{remote}` / `{agent_path}` placeholders, which
/// are substituted at spawn time. Keeping these as templates lets us share one
/// `spawn_direct_copy` implementation across `docker` / `podman`.
#[derive(Clone, Debug)]
pub struct DirectCopyPlan {
    /// Ordered list of arch-detection commands. The trimmed stdout of each
    /// step is interpolated as `{prev}` into the next; the last step's stdout
    /// must be a `"<OS>/<Arch>"` line.
    ///
    /// (Docker / Podman need two steps — `inspect` the container to get the
    /// image ID, then `image inspect` to get OS/Arch. The container itself
    /// doesn't expose `.Os` / `.Architecture` in its template namespace.)
    pub arch_detect_pipeline: Vec<Vec<String>>,
    /// Substitute `{local}` (host-side path to agent) and `{remote}`
    /// (target-side destination path).
    pub copy_cmd: Vec<String>,
    /// Substitute `{agent_path}` (target-side path). Stdin/stdout become the
    /// RPC channel; stderr is logged. Must produce a bidirectional pipe.
    pub exec_cmd: Vec<String>,
    pub label: String,
}

// ---------------------------------------------------------------------------
// Transport command builders
// ---------------------------------------------------------------------------

/// Build a `transport_cmd` for an ssh-based remote session, with
/// application-level keepalive enabled so that idle TCP connections aren't
/// silently killed by NAT / firewalls / load balancers. When `forward_agent`
/// is true, also adds `-A` so SSH agent forwarding is enabled.
pub fn ssh_transport_cmd(host: &str, forward_agent: bool) -> Vec<String> {
    let mut v = vec![
        "ssh".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=3".to_string(),
    ];
    if forward_agent {
        v.push("-A".to_string());
    }
    v.push(host.to_string());
    v
}

/// `docker exec -i [-u <user>] <container>` — the bootstrap script is appended
/// as the final argv element by the caller.
pub fn docker_transport_cmd(container: &str, user: Option<&str>) -> Vec<String> {
    let mut v = vec!["docker".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        v.push("-u".to_string());
        v.push(u.to_string());
    }
    v.push(container.to_string());
    v
}

/// `podman exec -i [-u <user>] <container>`.
pub fn podman_transport_cmd(container: &str, user: Option<&str>) -> Vec<String> {
    let mut v = vec!["podman".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        v.push("-u".to_string());
        v.push(u.to_string());
    }
    v.push(container.to_string());
    v
}

/// `kubectl exec -i [-c=…] <pod> [--context=…] [-n=…] --`.
///
/// Global flags (`--context`, `--namespace`) follow the `exec` subcommand
/// rather than precede `kubectl`, because some kubectl wrappers (notably
/// orbstack) reject flags before the plugin name.
pub fn kube_transport_cmd(
    context: Option<&str>,
    namespace: Option<&str>,
    pod: &str,
    container: Option<&str>,
) -> Vec<String> {
    let mut v = vec!["kubectl".to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(c) = container {
        v.push(format!("--container={}", c));
    }
    v.push(pod.to_string());
    if let Some(c) = context {
        v.push(format!("--context={}", c));
    }
    if let Some(n) = namespace {
        v.push(format!("--namespace={}", n));
    }
    v.push("--".to_string());
    v
}

/// Direct-copy plan for `docker`. `docker inspect` reports OS and architecture
/// from the image's manifest, which is exactly what we need — no shell in the
/// container required.
pub fn docker_direct_copy_plan(container: &str, user: Option<&str>) -> DirectCopyPlan {
    direct_copy_plan_for("docker", container, user)
}

/// Same shape as `docker_direct_copy_plan`. `podman cp` / `podman inspect` /
/// `podman exec` are CLI-compatible with their docker counterparts.
pub fn podman_direct_copy_plan(container: &str, user: Option<&str>) -> DirectCopyPlan {
    direct_copy_plan_for("podman", container, user)
}

fn direct_copy_plan_for(program: &str, container: &str, user: Option<&str>) -> DirectCopyPlan {
    // Step 1: container → image ID. Step 2: image ID → "<Os>/<Architecture>".
    // Container JSON doesn't expose Os/Architecture; the underlying image does.
    let arch_detect_pipeline = vec![
        vec![
            program.to_string(),
            "inspect".to_string(),
            "--format={{.Image}}".to_string(),
            container.to_string(),
        ],
        vec![
            program.to_string(),
            "image".to_string(),
            "inspect".to_string(),
            "--format={{.Os}}/{{.Architecture}}".to_string(),
            "{prev}".to_string(),
        ],
    ];
    let copy_cmd = vec![
        program.to_string(),
        "cp".to_string(),
        "{local}".to_string(),
        format!("{}:{{remote}}", container),
    ];
    let mut exec_cmd = vec![program.to_string(), "exec".to_string(), "-i".to_string()];
    if let Some(u) = user {
        exec_cmd.push("-u".to_string());
        exec_cmd.push(u.to_string());
    }
    exec_cmd.push(container.to_string());
    exec_cmd.push("{agent_path}".to_string());
    DirectCopyPlan {
        arch_detect_pipeline,
        copy_cmd,
        exec_cmd,
        label: format!("{}:{}", program, container),
    }
}

// ---------------------------------------------------------------------------
// Spawned agent plumbing
// ---------------------------------------------------------------------------

pub type DynStream =
    tokio_duplex::Duplex<Box<dyn AsyncRead + Send + Unpin>, Box<dyn AsyncWrite + Send + Unpin>>;

/// Erased stderr source, for transports whose stderr isn't consumed during
/// the handshake and still needs a reader attached.
pub type DynStderr = Box<dyn AsyncRead + Send + Unpin>;

/// Result of spawning an agent subprocess with a bidirectional RPC stream.
pub struct SpawnedAgent {
    pub child: tokio::process::Child,
    pub stderr: Option<DynStderr>,
    pub stream: DynStream,
    /// Askpass listener tied to the ssh process; dropped when the connection
    /// tears down. `None` for transports that can't prompt.
    pub askpass: Option<crate::askpass::listener::AskpassListener>,
}

pub fn make_stream(
    reader: BufReader<tokio::process::ChildStdout>,
    stdin: tokio::process::ChildStdin,
) -> DynStream {
    let rx: Box<dyn AsyncRead + Send + Unpin> = Box::new(reader);
    let tx: Box<dyn AsyncWrite + Send + Unpin> = Box::new(stdin);
    tokio_duplex::Duplex::new(rx, tx)
}

/// Spawn a task that reads lines from `stderr` and forwards them to the log
/// sink, so transport diagnostics surface in real time.
pub fn spawn_stderr_reader<R: AsyncRead + Unpin + Send + 'static>(
    stderr: R,
    log: Arc<dyn ConnectLog>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    log.log(line.trim_end().to_string());
                }
            }
        }
    });
}

/// Spawn an agent according to `spec`. Single entry point over the three
/// transport styles; returns a connection whose `stream` is ready for RPC.
pub async fn spawn(
    spec: &SpawnSpec,
    extra_path: &[String],
    agent_resolver: &dyn AgentResolver,
    askpass_provider: Arc<dyn AskpassProvider>,
    log: Arc<dyn ConnectLog>,
) -> Result<SpawnedAgent, Error> {
    match spec {
        SpawnSpec::Bootstrap {
            transport_cmd,
            askpass,
            shell_join,
            ..
        } => {
            spawn_bootstrap(
                transport_cmd,
                *askpass,
                *shell_join,
                extra_path,
                agent_resolver,
                askpass_provider,
                log,
            )
            .await
        }
        SpawnSpec::DirectCopy(plan) => {
            spawn_direct_copy(plan, extra_path, agent_resolver, log).await
        }
        SpawnSpec::CustomShell {
            command,
            skip_bootstrap,
            ..
        } => spawn_custom_shell(command, *skip_bootstrap, agent_resolver, log).await,
    }
}

/// Spawn a bootstrap-style transport (SSH / docker exec / kubectl exec / …),
/// pipe the embedded `bootstrap.sh` into it, and negotiate agent upload.
/// `enable_askpass` wires up SSH_ASKPASS for transports that may prompt for a
/// password (SSH); daemon-mediated transports (docker / kubectl etc.) skip it.
async fn spawn_bootstrap(
    transport_cmd: &[String],
    enable_askpass: bool,
    shell_join: bool,
    extra_path: &[String],
    agent_resolver: &dyn AgentResolver,
    askpass_provider: Arc<dyn AskpassProvider>,
    log: Arc<dyn ConnectLog>,
) -> Result<SpawnedAgent, Error> {
    let (program, args) = transport_cmd
        .split_first()
        .ok_or_else(|| Error::custom("empty transport command"))?;
    let program = crate::shell::resolve_program(program, extra_path);

    // The script reads `NEWT_RUST_LOG` from its own environment. Inject the
    // assignment as the first line of the script body so it survives transport
    // boundaries that don't propagate env vars (e.g. `docker exec` without `-e`).
    let mut script_body = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_resolver.agent_hash()?);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        let escaped_val = rust_log.replace('\'', "'\\''");
        script_body = format!("NEWT_RUST_LOG='{}'\n{}", escaped_val, script_body);
    }

    let askpass_listener = if enable_askpass {
        Some(crate::askpass::listener::spawn(askpass_provider)?)
    } else {
        None
    };
    let askpass_binary = if enable_askpass {
        Some(agent_resolver.find_local_agent_binary()?)
    } else {
        None
    };

    log.log(format!(
        "Spawning: {} {}",
        program.display(),
        args.join(" ")
    ));

    let mut cmd = tokio::process::Command::new(&program);
    cmd.no_console_window();
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if shell_join {
        // SSH joins its trailing argv with spaces and re-runs the result inside
        // a shell on the remote. Quote everything into one argv element so the
        // remote sees `sh -c '<script>'`.
        let escaped = script_body.replace('\'', "'\\''");
        cmd.arg(format!("sh -c '{}'", escaped));
    } else {
        // `docker exec` / `podman exec` / `kubectl exec` / custom transports
        // `execvp` their argv directly. Pass `sh`, `-c`, `<script>` as three
        // separate elements.
        cmd.arg("sh").arg("-c").arg(&script_body);
    }
    if let (Some(askpass_binary), Some(listener)) = (&askpass_binary, &askpass_listener) {
        cmd.env("SSH_ASKPASS", askpass_binary)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("NEWT_ASKPASS_SOCK", &listener.socket_path);
    }
    set_parent_death_signal(&mut cmd);
    let child = cmd.spawn()?;
    perform_bootstrap_handshake(child, askpass_listener, agent_resolver, log).await
}

/// Run the `NEWT:READY` / `NEWT:NEED` negotiation on a freshly-spawned child
/// whose stdin/stdout will become the RPC channel. Shared between the
/// argv-appending bootstrap path and the env-var-based custom-shell path.
async fn perform_bootstrap_handshake(
    mut child: tokio::process::Child,
    askpass_listener: Option<crate::askpass::listener::AskpassListener>,
    agent_resolver: &dyn AgentResolver,
    log: Arc<dyn ConnectLog>,
) -> Result<SpawnedAgent, Error> {
    log.log("Process spawned, waiting for bootstrap...".to_string());

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Capture recent stderr lines locally so a failed bootstrap can surface
    // the most relevant diagnostic in its error, while still forwarding
    // everything to the log sink.
    let stderr_tail: Arc<parking_lot::Mutex<Option<String>>> =
        Arc::new(parking_lot::Mutex::new(None));
    struct TailLog {
        inner: Arc<dyn ConnectLog>,
        tail: Arc<parking_lot::Mutex<Option<String>>>,
    }
    impl ConnectLog for TailLog {
        fn log(&self, line: String) {
            *self.tail.lock() = Some(line.clone());
            self.inner.log(line);
        }
    }
    spawn_stderr_reader(
        stderr,
        Arc::new(TailLog {
            inner: log.clone(),
            tail: stderr_tail.clone(),
        }),
    );

    // Read status line, skipping any noise from .bashrc etc.
    let mut reader = BufReader::new(stdout);
    let status_line = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Connection closed — give the stderr reader a beat to flush the
            // last diagnostic line, then use it for context.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let detail = stderr_tail
                .lock()
                .as_ref()
                .map(|l| format!(": {}", l))
                .unwrap_or_default();
            return Err(Error::custom(format!(
                "remote connection closed before bootstrap completed{}",
                detail
            )));
        }
        let trimmed = line.trim();
        if trimmed.starts_with("NEWT:") {
            break trimmed.to_string();
        }
        log.log(format!("bootstrap: {}", trimmed));
    };
    let status_line = status_line.as_str();

    if status_line == "NEWT:READY" {
        log.log("Agent ready".to_string());
        Ok(SpawnedAgent {
            stream: make_stream(reader, stdin),
            child,
            stderr: None,
            askpass: askpass_listener,
        })
    } else if let Some(need_rest) = status_line.strip_prefix("NEWT:NEED:") {
        // Format: NEWT:NEED:<triple>:<caps> where caps is comma-separated
        let (triple, caps_str) = need_rest.split_once(':').unwrap_or((need_rest, ""));
        let caps: Vec<&str> = caps_str.split(',').filter(|s| !s.is_empty()).collect();
        let has_gzip = caps.contains(&"gzip");

        log.log(format!(
            "Agent needs upload for {} (caps: {})",
            triple,
            if caps.is_empty() { "none" } else { caps_str }
        ));
        let binary_path = agent_resolver.find_agent_binary(triple)?;
        let binary_data = tokio::fs::read(&binary_path).await?;
        let raw_size = binary_data.len();

        let (upload_data, encoding) = if has_gzip {
            use flate2::Compression;
            use flate2::write::GzEncoder;
            use std::io::Write;

            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&binary_data)?;
            let compressed = encoder.finish()?;
            log.log(format!(
                "Compressed {} → {} bytes ({:.0}%)",
                raw_size,
                compressed.len(),
                compressed.len() as f64 / raw_size as f64 * 100.0
            ));
            (compressed, "gzip")
        } else {
            (binary_data, "raw")
        };

        log.log(format!(
            "Uploading agent ({} bytes, {})...",
            upload_data.len(),
            encoding
        ));
        stdin
            .write_all(format!("{} {}\n", upload_data.len(), encoding).as_bytes())
            .await?;
        stdin.write_all(&upload_data).await?;
        stdin.flush().await?;
        log.log("Agent uploaded".to_string());

        Ok(SpawnedAgent {
            stream: make_stream(reader, stdin),
            child,
            stderr: None,
            askpass: askpass_listener,
        })
    } else if let Some(error) = status_line.strip_prefix("NEWT:ERROR:") {
        Err(Error::custom(format!("remote bootstrap error: {}", error)))
    } else {
        Err(Error::custom(format!(
            "unexpected bootstrap response: {}",
            status_line
        )))
    }
}

/// Bootstrapless launch: detect the target architecture via the daemon's
/// `inspect` command, copy the matching agent binary in with `<engine> cp`, and
/// exec it directly. Used for distroless / `FROM scratch` containers that have
/// no shell to run `bootstrap.sh`.
async fn spawn_direct_copy(
    plan: &DirectCopyPlan,
    extra_path: &[String],
    agent_resolver: &dyn AgentResolver,
    log: Arc<dyn ConnectLog>,
) -> Result<SpawnedAgent, Error> {
    // 1. Arch detection — run the pipeline; each step's stdout is piped into
    //    the next as `{prev}`.
    if plan.arch_detect_pipeline.is_empty() {
        return Err(Error::custom("empty arch_detect_pipeline"));
    }
    let mut prev: String = String::new();
    for step in &plan.arch_detect_pipeline {
        let resolved: Vec<String> = step.iter().map(|a| a.replace("{prev}", &prev)).collect();
        let (prog, args) = resolved
            .split_first()
            .ok_or_else(|| Error::custom("empty arch_detect step"))?;
        let prog = crate::shell::resolve_program(prog, extra_path);
        log.log(format!(
            "Detecting target arch: {} {}",
            prog.display(),
            args.join(" ")
        ));
        let out = tokio::process::Command::new(&prog)
            .no_console_window()
            .args(args)
            .output()
            .await
            .map_err(|e| Error::custom(format!("arch_detect failed to spawn: {}", e)))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Error::custom(format!(
                "arch detection failed (exit {:?}): {}",
                out.status.code(),
                stderr
            )));
        }
        prev = String::from_utf8_lossy(&out.stdout).trim().to_string();
    }
    let line = prev.as_str();
    let (os, arch) = line.split_once('/').ok_or_else(|| {
        Error::custom(format!(
            "could not parse arch_detect output {:?} (expected `<os>/<arch>`)",
            line
        ))
    })?;
    let triple = crate::agent_resolver::triple_from_os_arch(os, arch).ok_or_else(|| {
        Error::custom(format!(
            "unsupported target: os={:?}, arch={:?} (no matching agent triple)",
            os, arch
        ))
    })?;
    log.log(format!("Target reports {}/{} → {}", os, arch, triple));

    // 2. Resolve local binary.
    let local_binary = agent_resolver.find_agent_binary(&triple)?;
    let agent_hash = agent_resolver.agent_hash()?;
    let remote_path = format!("/tmp/newt-agent-{}", agent_hash);

    // The source binary on disk is typically mode 644 (cargo-zigbuild output,
    // or unpacked from a tarball that lost the bit). `docker cp` preserves
    // mode, so a 644 file inside the container can't be exec'd. In the
    // bootstrap path, `bootstrap.sh` `chmod +x`'s the upload before exec;
    // bootstrapless has no such hook in the container, so we stage a +x copy
    // host-side and `docker cp` that.
    let staged = tempfile::Builder::new()
        .prefix("newt-agent-")
        .tempfile()
        .map_err(|e| Error::custom(format!("could not create temp file: {}", e)))?;
    tokio::fs::copy(&local_binary, staged.path())
        .await
        .map_err(|e| Error::custom(format!("could not stage agent for copy: {}", e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| Error::custom(format!("could not chmod staged agent: {}", e)))?;
    }

    // 3. Copy. The destination is always /tmp/… in the target.
    let local_str = staged.path().to_string_lossy().to_string();
    let copy_argv: Vec<String> = plan
        .copy_cmd
        .iter()
        .map(|a| {
            a.replace("{local}", &local_str)
                .replace("{remote}", &remote_path)
        })
        .collect();
    let (copy_program, copy_args) = copy_argv
        .split_first()
        .ok_or_else(|| Error::custom("empty copy_cmd"))?;
    let copy_program = crate::shell::resolve_program(copy_program, extra_path);
    log.log(format!(
        "Copying agent: {} {}",
        copy_program.display(),
        copy_args.join(" ")
    ));
    let copy_status = tokio::process::Command::new(&copy_program)
        .no_console_window()
        .args(copy_args)
        .output()
        .await
        .map_err(|e| Error::custom(format!("cp failed to spawn: {}", e)))?;
    if !copy_status.status.success() {
        let stderr = String::from_utf8_lossy(&copy_status.stderr)
            .trim()
            .to_string();
        return Err(Error::custom(format!(
            "agent copy failed (exit {:?}): {}",
            copy_status.status.code(),
            stderr
        )));
    }
    log.log("Agent copied".to_string());

    // 4. Exec.
    let exec_argv: Vec<String> = plan
        .exec_cmd
        .iter()
        .map(|a| a.replace("{agent_path}", &remote_path))
        .collect();
    let (exec_program, exec_args) = exec_argv
        .split_first()
        .ok_or_else(|| Error::custom("empty exec_cmd"))?;
    let exec_program = crate::shell::resolve_program(exec_program, extra_path);
    log.log(format!(
        "Exec: {} {}",
        exec_program.display(),
        exec_args.join(" ")
    ));
    let mut cmd = tokio::process::Command::new(&exec_program);
    cmd.no_console_window();
    cmd.args(exec_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    set_parent_death_signal(&mut cmd);
    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stream = make_stream(BufReader::new(stdout), stdin);

    Ok(SpawnedAgent {
        child,
        stream,
        stderr: Some(Box::new(stderr)),
        askpass: None,
    })
}

/// Run a user-supplied shell command locally via `sh -c <command>`. The
/// bootstrap script is exposed as `NEWT_BOOTSTRAP` so the user can splice it
/// in (`ssh host "$NEWT_BOOTSTRAP"`, `bash -c "$NEWT_BOOTSTRAP"`, etc.).
///
/// If `skip_bootstrap` is false (the default), we still run the bootstrap
/// handshake on the resulting stdin/stdout — so any sane interpolation of
/// `$NEWT_BOOTSTRAP` Just Works. If true, we hand the pipe directly to RPC,
/// assuming the user produced a ready agent out of band.
async fn spawn_custom_shell(
    command: &str,
    skip_bootstrap: bool,
    agent_resolver: &dyn AgentResolver,
    log: Arc<dyn ConnectLog>,
) -> Result<SpawnedAgent, Error> {
    let script = BOOTSTRAP_SCRIPT.replace("__NEWT_HASH__", &agent_resolver.agent_hash()?);

    log.log(format!(
        "Running custom command (skip_bootstrap={}): sh -c {:?}",
        skip_bootstrap, command
    ));
    let (shell, shell_args) = crate::shell::run_via_shell(command);
    let mut cmd = tokio::process::Command::new(shell);
    cmd.no_console_window();
    cmd.args(shell_args)
        .env("NEWT_BOOTSTRAP", &script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("NEWT_RUST_LOG", rust_log);
    }
    set_parent_death_signal(&mut cmd);
    let mut child = cmd.spawn()?;

    if skip_bootstrap {
        // Trust the user — pipe straight to RPC.
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        Ok(SpawnedAgent {
            child,
            stream: make_stream(BufReader::new(stdout), stdin),
            stderr: Some(Box::new(stderr)),
            askpass: None,
        })
    } else {
        perform_bootstrap_handshake(child, None, agent_resolver, log).await
    }
}

#[cfg(test)]
mod tests {
    use super::BOOTSTRAP_SCRIPT;

    /// `bootstrap.sh` is embedded verbatim via `include_str!` and run by a
    /// remote POSIX `sh`, which fails on `\r`. `.gitattributes` pins it to
    /// LF; this guards against a regression (a checkout that smuggled CRLF
    /// in, or that pin being lost).
    #[test]
    fn bootstrap_script_is_lf_only() {
        assert!(
            !BOOTSTRAP_SCRIPT.contains('\r'),
            "bootstrap.sh contains CR — must be LF-only (check .gitattributes / autocrlf)"
        );
    }
}
