# pty-process

Stripped-down fork of [pty-process](https://crates.io/crates/pty-process) by Jesse Luehrs ([@doy](https://github.com/doy)), originally published under the MIT license.

The upstream crate provides a full-featured wrapper around PTY allocation and process spawning. This fork retains only the subset used by Newt: allocating a PTY, spawning a child process attached to it, and splitting the master into async read/write halves for use with tokio.

Licensed under the MIT license. See [LICENSE](LICENSE) for details.
