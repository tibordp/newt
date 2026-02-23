# Newt - Development Guidelines

Newt is a dual-pane file manager desktop app (think Midnight Commander / Altap Salamander meets modern UI). Built with Tauri 2 (Rust backend, React/TypeScript frontend).

## Architecture

```
Frontend (React/TS)  ──Tauri IPC──>  Tauri Backend (Rust)  ──stdin/stdout RPC──>  Agent (newt-agent)
                                          │                                            │
                                     MainWindowState                          Filesystem, PTY,
                                     (source of truth)                        Operations execution
```

- **Frontend** (`src/`): React UI — dual file panes, terminal (xterm.js), modals, operations panel.
- **Tauri backend** (`src-tauri/`): Window management, state (`MainWindowState`), command handlers. Pushes state to frontend via `UpdatePublisher` (treediff → Immer-compatible patches).
- **Agent** (`libs/newt-agent/`): Separate long-lived process spawned by Tauri. Handles all filesystem I/O, PTY/terminal management, and long-running operation execution (copy/move/delete). Communicates via custom binary RPC protocol (bincode over stdin/stdout). Isolated from UI so heavy I/O doesn't block rendering.
- **Common library** (`libs/newt-common/`): Shared types, RPC protocol, filesystem/terminal trait abstractions (Local impl in agent, Remote proxy in Tauri).

## UX: Keyboard-Centric Design

This is a keyboard-centric app. All UX decisions should keep efficient keyboard navigation firmly in mind. Pay special attention to focus management — dialogs should always have a sensible default choice focused, and focus must be properly restored when dialogs close.

## State Ownership

Be intentional about where state is kept:

- **React (local state)**: Purely local/ephemeral state — form inputs, hover states, UI-only toggles that don't affect anything outside the component.
- **Rust (MainWindowState)**: Any state with app-wide consequences. This is the primary source of truth, pushed to the frontend via the update/patch system.
- **Agent**: Some state can live in the agent process if it makes sense (e.g. per-operation progress tracking), but the bar is higher — prefer Rust unless the agent is the natural owner.
