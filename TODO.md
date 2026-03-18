# TODO

## Askpass support for SFTP VFS

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (rename, touch, create_directory, etc.)
- Opening remote files locally (download then xdg-open)

## Generalized remote session transport

Currently remote sessions only support SSH. The architecture (agent binary + stdin/stdout RPC) could work with docker exec, kubectl exec, and similar transports. Requires extracting connection establishment from the Tauri backend into newt-common and handling askpass-style prompts generically. Connection profiles already support a generic "remote" type.

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- Rework MIME handling so we fall back to file extension rather than binary sniffing when we can't read the file cheaply
- Per-byte progress tracking for copying
- Remount VFS when navigating to it from history - save mount data in history
- Persist column widths across sessions

## Compute dir sizes recursively (with caching)

Cache invalidation is the hard problem — filesystem events don't bubble up from subdirectories reliably.
