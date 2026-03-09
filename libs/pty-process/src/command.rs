/// Wrapper around [`tokio::process::Command`] that spawns processes attached
/// to a pty.
pub struct Command {
    inner: tokio::process::Command,
}

impl Command {
    pub fn new<S: AsRef<std::ffi::OsStr>>(program: S) -> Self {
        Self {
            inner: tokio::process::Command::new(program),
        }
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.inner.args(args);
        self
    }

    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.inner.env(key, val);
        self
    }

    pub fn kill_on_drop(&mut self, kill_on_drop: bool) -> &mut Self {
        self.inner.kill_on_drop(kill_on_drop);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<std::ffi::OsStr>,
        V: AsRef<std::ffi::OsStr>,
    {
        self.inner.envs(vars);
        self
    }

    pub fn current_dir<P: AsRef<std::path::Path>>(&mut self, dir: P) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }

    /// Spawn the command attached to the given pty. The child's
    /// stdin/stdout/stderr are all connected to the pty, and it becomes
    /// the session leader with the pty as its controlling terminal.
    pub fn spawn(&mut self, pts: &crate::Pts) -> crate::Result<tokio::process::Child> {
        let session_leader = pts.0.session_leader();

        // Safety: the closure only uses async-signal-safe syscalls (setsid,
        // open, ioctl, dup2, close). The CString allocation for the slave
        // path happens in session_leader() above, before the closure is
        // returned, so nothing allocates between fork and exec.
        unsafe { self.inner.pre_exec(session_leader) };

        Ok(self.inner.spawn()?)
    }
}
