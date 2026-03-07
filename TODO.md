# TODO

## SSH Askpass

SSH password/passphrase/host-key-confirmation support via `SSH_ASKPASS`.

- Create a `socketpair(AF_UNIX, SOCK_STREAM)` before spawning SSH
- Pass one end as fd 3 to the SSH child process (clear `CLOEXEC` via `pre_exec`)
- Set `SSH_ASKPASS_REQUIRE=force` and `SSH_ASKPASS` to a small helper script
- Helper script: `printf '%s\n%s\n' "${SSH_ASKPASS_PROMPT:-}" "$1" >&3; read -r resp <&3; printf '%s\n' "$resp"`
- `SSH_ASKPASS_PROMPT` env var (OpenSSH 8.4+) distinguishes prompt types:
  - unset/empty: secret input (password, passphrase) — mask with `*`
  - `confirm`: yes/no prompt (host key verification)
  - `none`: informational, no input needed
- Tauri side: accept on the socketpair, read prompt type + prompt text, show modal dialog, write response back
- Reuse for both agent connection (`ssh host`) and SFTP VFS connections
- Ship the askpass helper as a bundled resource (or write a temp script at runtime)

## SFTP VFS

Remote filesystem browsing via SFTP, using the system OpenSSH to respect `~/.ssh/config`.

- Spawn `ssh -s sftp hostname` as a subprocess (stdin/stdout as transport)
- Use `openssh-sftp-client` crate — accepts `ChildStdin`/`ChildStdout` directly
- Implement `Vfs` trait on top of the SFTP client:
  - Direct mappings: `list_files`, `file_info`, `file_details`, `read_range`, `open_read_async`, `overwrite_async`, `create_directory`, `remove_file`, `remove_dir`, `rename`, `create_symlink`, `get_metadata`, `set_metadata`
  - `poll_changes`: use `VfsChangeNotifier` (self-notify on mutations, no remote watching)
  - `fs_stats`/`available_space`: `statvfs@openssh.com` extension (OpenSSH only)
  - `hard_link`: `hardlink@openssh.com` extension (OpenSSH only)
  - `copy_within`: not supported (no server-side copy in SFTP v3; let operations engine handle it)
  - `remove_tree`: not supported (walk + delete bottom-up, or let operations engine handle it)
  - `open_read_sync`/`overwrite_sync`: not supported (use async paths)
  - `File.user/group`: uid/gid as integers only (`UserGroup::Id`)
  - `File.is_hidden`: derive from name starting with `.`
- Wire up SSH Askpass (fd 3 socketpair) for password prompts
- Add `MountRequest::Sftp { hostname: String }` variant
- Add `SftpVfsDescriptor` with appropriate capabilities

## Archive VFS

Read-only (initially) VFS for browsing archive contents (tar, zip, etc.) as a mounted filesystem.
