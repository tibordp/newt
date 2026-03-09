# Newt

A dual-pane file manager for Linux and macOS. Keyboard-centric, built with Tauri 2 (Rust backend, React/TypeScript frontend).

## Features

- Dual-pane file browsing
- Integrated terminal (xterm.js)
- File operations: copy, move, delete, set permissions
- Remote sessions over SSH via a lightweight agent binary
- VFS mounts for S3 and SFTP
- Elevated operations via pkexec

## Architecture

```
Frontend (React/TS)  ──Tauri IPC──>  Tauri Backend (Rust)  ──RPC──>  Agent
```

- **`src/`** — React frontend: dual panes, terminal, modals, operations panel
- **`src-tauri/`** — Tauri backend: window/state management, command handlers
- **`libs/newt-common/`** — Shared types, RPC protocol, VFS traits, operation logic
- **`libs/newt-agent/`** — Standalone binary for remote/elevated sessions

All filesystem and terminal functionality goes through traits with local (in-process) and remote (RPC to agent) implementations. The frontend is a rendering layer driven by state pushed from Rust via treediff/Immer patches.

## Building

Requires Rust, Node.js, and Yarn.

```sh
yarn install
cargo tauri dev
```
