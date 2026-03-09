# TODO

## Askpass support for SFTP VFS

## Archive VFS

Read-only (initially) VFS for browsing archive contents (tar, zip, etc.) as a mounted filesystem. Major feature, probably its own crate for the juicy part, which is
world-class TAR support (with random access - idea is the same as ratarmount, we do one pass through the tar, build an index of file -> offset in decompressed stream plus
entire decompressor state at various checkpoints spaced ~every 10MB or so). This will allow us to efficiently do random byte range requests even through remoting without
having to unpack everything.

## Host VFS (local ↔ remote bridge)

Expose the Tauri-side (host) filesystem as a VFS accessible from the agent side, and vice versa. In a remote session, this lets you mount the local machine as a browsable pane and use the standard operation system for uploads/downloads with full progress, conflict resolution, and cancellation. In a local session, a remote VFS could be useful for copying into containers or other environments where an agent can be bootstrapped.

Requires registering a filesystem dispatcher on the Tauri side of the RPC channel (protocol is already symmetric, just no dispatchers registered on the local side today). Also unlocks: external drag-and-drop (drop → copy from host VFS), opening remote files locally (download then xdg-open), and having a proper local pane in remote sessions.

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- S3 connect dialog with the ability to pick profile or enter credentials manually.
- SFTP dialog not focusing when selected from the VFS dropdown
- Ability to unmount VFSes

## Customizability

## Compute dir sizes recursively (with caching)
