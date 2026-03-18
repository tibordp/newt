# TODO

## Askpass support for SFTP VFS

## Host VFS (local ↔ remote bridge) — DONE (basic)

Basic Remote VFS is implemented: in SSH sessions, the client-local filesystem is exposed as a mountable VFS on the agent side. Hairpin diversion routes list_files, poll_changes, read_range, read_file, and write_file directly through the Tauri backend, avoiding double network roundtrips. Gated behind `behavior.expose_local_fs` preference (default false for security).

Remaining work:
- Hairpin diversion for additional methods (rename, touch, create_directory, etc.)
- External drag-and-drop (drop → copy from host VFS)
- Opening remote files locally (download then xdg-open)

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- S3 connect dialog with the ability to pick profile or enter credentials manually.
- Rework MIME handling so we fall back to file extension rather than binary sniffing when we can't read the file cheaply
- Per-byte progress tracking for copying
- Remount VFS when navigating to it from history - save mount data in history
- Persist column widths across sessions

## Customizability — DONE (basic)

Settings dialog with schema-driven UI, custom widgets for complex preferences (columns, default sort), reset-to-default support. Column visibility/order and default sort are configurable. See Preferences section in FEATURE_DUMP.md.

## Compute dir sizes recursively (with caching)
