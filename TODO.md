# TODO

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (rename, touch, create_directory, etc.) — only `list_files`, `poll_changes`, `read_range`, `read_file`, `write_file` are diverted today.

## Generalized remote session transport

Internally `ConnectionTarget::Remote` already accepts an arbitrary `transport_cmd: Vec<String>`, but every call site builds it as `["ssh", host]`, and `spawn_remote` hardcodes `SSH_ASKPASS` / `SSH_ASKPASS_REQUIRE` env vars that only ssh respects. To support docker exec, kubectl exec, etc., we need:
- A way to specify a custom transport command from the UI / connection profile (the profile schema today is SSH/S3-only).
- A transport-agnostic mechanism for password / host-key prompts (the askpass plumbing assumes ssh's env-var protocol).

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Persist column widths across sessions (today they only persist for the lifetime of the session)

## Compute dir sizes recursively (with caching)

Cache invalidation is the hard problem — filesystem events don't bubble up from subdirectories reliably.
