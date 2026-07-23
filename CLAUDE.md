# Newt - Development Guidelines

Newt is a dual-pane orthodox file manager desktop app. Built with Tauri 2 (Rust backend, React/TypeScript frontend) for Mac, Linux and Windows. 

## Architecture

```
Frontend (React/TS)  ──Tauri IPC──>  Tauri Backend (Rust)  ──stdin/stdout RPC──>  Agent (newt-agent)
                                          │                                            │
                                     MainWindowState                          Filesystem, PTY,
                                     (source of truth)                        Operations execution
```

- **Frontend** (`src/`): React UI — dual file panes, terminal (xterm.js), modals, operations panel.
- **Tauri backend** (`src-tauri/`): Window management, state (`MainWindowState`), command handlers. Pushes state to frontend via `UpdatePublisher` (treediff → Immer-compatible patches).
- **Agent** (`libs/newt-agent/`): Separate binary for remoting scenarios (SSH, elevated/pkexec). Communicates with the Tauri backend via a custom binary RPC protocol (bincode over stdin/stdout).
- **Common library** (`libs/newt-common/`): Shared types, RPC protocol, filesystem/terminal/operation trait abstractions with both Local and Remote implementations.

### Sessions

One OS process hosts every window. A **session** is a MainWindow together with the viewer/editor windows opened from it: one `MainWindowState`, one connection (local, or one agent subprocess when remote/elevated), one set of VFS mounts, terminals, and operations. Sessions are fully independent of each other — a local session and a remote one coexist in the same process — but they *do* share the process, so nothing process-global (event emits, statics, per-session counters) is implicitly scoped to one session or window. The only genuinely app-wide state is preferences. Prefer "session" over "app" when describing scope: most state that feels app-wide is actually session-wide.

### Local vs Remote mode

All filesystem, terminal, and operation functionality is accessed through traits (`Filesystem`, `TerminalClient`, `OperationsClient`, etc.) defined in `newt-common`. These have two implementations:

- **Local mode** (default): The Tauri backend uses the Local implementations directly in-process. No agent subprocess is spawned. There is no serialization overhead.
- **Remote/Elevated mode**: The entire session runs on the remote host (or with elevated privileges). The Tauri backend uses Remote proxy implementations that forward calls over the binary RPC protocol to an agent subprocess (connected via SSH or pkexec). Terminals, file operations, and VFS mounts all originate from the agent side.

Both modes use the exact same traits, so the Tauri backend and frontend code is identical regardless of connection mode. The agent exists purely to run operations on a different host or with different privileges — it is not needed for process isolation in the local case.

### VFS (Virtual Filesystem)

Remote VFSes (S3, SFTP, Kubernetes, ...) are orthogonal to the session mode. A VFS is mounted into the `VfsRegistry` and accessed through the same `Filesystem` trait. In a local session, the VFS connection originates from the Tauri process; in a remote session, it originates from the agent — so e.g. an S3 mount in a remote session uses the remote host's AWS credentials and network.

### RPC boundary

All host↔agent and agent↔agent communication is **tightly coupled**. Every participant in a session is always an artifact built from the same source tree. The protocol is an internal distributed ABI, not a public API, an independently versioned service boundary, or an untrusted interoperability surface. 

Do not add protocol-version negotiation, backward-compatibility layers, tolerant decoding, or recovery paths for structurally invalid payloads. Underlying transport (SSH, ...) is responsible for ensuring the integrity. A payload that successfully crosses the transport boundary but cannot be decoded or violates protocol invariants indicates a programming error, corrupted process state, or a broken internal ABI contract. **Panic is the correct response.** Do not turn these failures into ordinary `Result` errors or attempt to continue the session.

Transport-level failures such as EOF, subprocess termination, or an unavailable connection may still be represented as operational errors where appropriate. This does not apply to malformed or semantically impossible protocol data. The agent lives only for the duration of the session, then terminates. 

`std::path::Path`/`PathBuf` must **never** appear on a serde-serialized type that crosses the agent↔host RPC boundary (anything sent via the `Communicator` or an `api`/`VfsDescriptor` payload). Use `newt_common::vfs::path::PathBuf` (implements `specta::Type` + serde) or `String`. Native conversion happens only on the side that physically owns the filesystem. Treat this as a litmus test whenever touching an RPC/`api` type or adding a path field — audit the whole type, don't symptom-fix one field (`agent_resolver` host-local paths are exempt — never serialized).

## UX: Keyboard-Centric Design

This is a keyboard-centric app. All UX decisions should keep efficient keyboard navigation firmly in mind.

### Focus Management

Focus management is critical — broken focus means the user has to reach for the mouse, which defeats the purpose of the app.

- **Dialog open**: Always auto-focus the most likely control (e.g. the text input in a rename dialog, the confirm button in a confirmation dialog). Use `autoFocus` or a ref-based `.focus()` in `useEffect`.
- **Dialog close**: Focus **must** return to the active pane/terminal. This is handled by `onCloseAutoFocus` on the Radix Dialog, which calls `refocusActivePane` (increments `focusGeneration` → Pane re-runs its focus effect). New dialogs must wire this up — never let focus drop to `<body>` after a dialog closes.
- **Between panes**: Tab switches panes. The active pane is tracked in `DisplayOptions.active_pane` (Rust state), not in React focus state.
- **Pane ↔ Terminal**: Focus ownership is tracked in `DisplayOptions.panes_focused`. Clicking a terminal or pressing the toggle shortcut updates this in Rust, and the frontend follows.

### Footgun: Window-targeted events

All sessions share **one process** (see Sessions above).

Tauri 2's `Emitter::emit` **always emits to every target**, whatever you call it on: `window.emit(…)` is *not* window-scoped. Anything destined for one window must be `window.emit_to(window.label(), …)`. Only genuinely app-wide events (like preference changes) can use `emit`.

## State Ownership

Be intentional about where state is kept:

- **React (local state)**: Purely local/ephemeral state — form inputs, hover states, drag tracking, scroll position. If it doesn't need to survive a re-render from Rust or be visible to other components, it belongs here.
- **Rust (MainWindowState)**: Any state with session-wide consequences — this is the primary source of truth, pushed to the frontend via `UpdatePublisher` (treediff → Immer patches). The frontend is a **rendering layer**: it receives state and renders it, it does not own session state.
- **Agent**: Some state can live in the agent process if it makes sense (e.g. per-operation progress tracking), but the bar is higher — prefer Rust unless the agent is the natural owner.

### Modals and Dialogs

**Never use React state to control dialog visibility.** All modals are driven by `MainWindowState.modal` (`ModalState` / `ModalDataKind` in Rust). To add a new dialog:

1. Add a variant to `ModalDataKind` in Rust with whatever data the dialog needs.
2. Add a case in the `dialog()` command handler to populate it.
3. Use the `cmd_dialog!` macro to create a `cmd_*` entry point (keyboard/palette trigger).
4. Render the new dialog in the frontend's `ModalContent` component, reading props from the pushed state.

The `cmd_*` middleware automatically closes any open modal before dispatching, so individual commands never need to manage modal lifecycle. The frontend's `Dialog.Root` opens/closes based on `remoteState.modal` — there is no `useState` for open/closed.

### Adding New Session-Wide State

When adding state that affects the session's UI beyond a single component (e.g. a new panel, a toggle, a mode):

1. Add the field to the appropriate Rust struct (`MainWindowState`, `DisplayOptions`, etc.).
2. Derive/implement `Serialize` so the patch system picks it up.
3. Modify it via `with_update` / `with_update_async` in a command handler.
4. Read it from `remoteState` on the frontend — do not duplicate it into `useState`.

## Async and cancellability

We prefer async code over sync code, even at the slight expense of efficiency. The main reason for this is ease of cancellation (by dropping the pending future). We are willing to go above and beyond to make things async-friendly, including reimplementation of popular crates. Dropping a future to cancel is preferred over cancellation tokens, though cancellation tokens are acceptable. 

In general all file operations, both on the read path and the write path should be cancellable and cancellation should propagate across the RPC boundary when using remoting. This is not always 100% possible - we are often bridging sync and async code and sometimes a synchronous operation may be legitimately stuck on a syscall or cancellation may not be feasible. When this happens we need to consider the following guidance:

1. If an operation is doing something potentially destructive (like overwriting a file) or expensive (listing billions of blobs on S3), we should not remove a visual indicator that an operation is in progress. User may want to go and kill the process to terminate it. 
2. We should not continue pumping bytes over the RPC boundary when the receiver no longer cares about them, even if producing them cannot be cleanly interrupted
3. If a stuck operation is doing something innocuous or inert and it's only consuming a blocking-pool thread (never a runtime worker — stuck sync work must live in `spawn_blocking`), it's fine to leave it orphaned, but it should not prevent other operations from making progress.

## Communication

When a task or direction is unclear — especially around architecture or design intent — stop and ask rather than guessing. The user likely has a specific vision; don't fill in the blanks with assumptions.

## Code Style

- **Comments:** sparse — only non-obvious rationale (a trap, a why). Don't narrate what the code says, and don't repeat a point in both a comment and an adjacent doc. `///` on a `clap` field is `--help` text: one short line.
- **Match existing patterns over ceremony:** before adding auth/validation/guards/error-handling, check how analogous code here handles the same boundary; justify any deviation with a concrete threat rather than "to be safe".
- **Sync↔async bridging is a smell:** `block_on`, `spawn_blocking` wrappers, blocking inside async (or vice versa) need explicit justification — it's not always avoidable, but reach for the async-native or sync-native path first.
- **No reflexive sleeps/timeouts/retries:** sprinkling `sleep`s, arbitrary timeouts, or "short" retry loops to paper over a race is almost always wrong and needs very high justification. Fix the synchronization (await the actual signal/handle) instead.

## Git Commits

When adding new features or significantly reworking existing ones - make sure TODO.md and FEATURE_DUMP.md are updated. These docs are agent-consumption material, not user-facing copy: word the updates yourself in the style of the surrounding entries, don't ask the user for phrasing. Concretely: delete TODO.md items the change resolves, and slot new behaviour into the relevant FEATURE_DUMP.md section (and the settings reference, if a new preference was added). TODO.md should always be actionable future work, known limitations and implementation details belong into FEATURE_DUMP.md or code comments. Do not preface pending work with a list of things that were already done. I repeat: *Do not leave junk in TODO.md, remove it when done!*

Run `git hook run pre-commit` after staging and before commiting. Re-stage the changes it makes, if any, and fix the issues it surfaces. Never `git stash` to satisfy the hook's whole-tree gate; if several logical commits are pending, sequence them instead. With regards to commit hygiene, it is fine for small drive-by or tangentially related changes to piggyback on the larger change; do not waste effort to cleanly split them, just commit as a single commit and add a note to the commit message.

Do not add `Co-Authored-By` lines to commit messages.
