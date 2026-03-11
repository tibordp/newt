# TODO

## Askpass support for SFTP VFS

## Host VFS (local ↔ remote bridge)

Expose the Tauri-side (host) filesystem as a VFS accessible from the agent side, and vice versa. In a remote session, this lets you mount the local machine as a browsable pane and use the standard operation system for uploads/downloads with full progress, conflict resolution, and cancellation. In a local session, a remote VFS could be useful for copying into containers or other environments where an agent can be bootstrapped.

Requires registering a filesystem dispatcher on the Tauri side of the RPC channel (protocol is already symmetric, just no dispatchers registered on the local side today). Also unlocks: external drag-and-drop (drop → copy from host VFS), opening remote files locally (download then xdg-open), and having a proper local pane in remote sessions.

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- S3 connect dialog with the ability to pick profile or enter credentials manually.
- Rework MIME handling so we fall back to file extension rather than binary sniffing when we can't read the file cheaply
- Per-byte progress tracking for copying
- ErrorBoundary
- Remount VFS when navigating to it from history - save mount data in history

## Customizability

## Compute dir sizes recursively (with caching)
