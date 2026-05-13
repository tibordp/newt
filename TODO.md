# TODO

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (rename, touch, create_directory, etc.) — only `list_files`, `poll_changes`, `read_range`, `read_file`, `write_file` are diverted today.

## Generalized remote session transport

Implemented: SSH, pkexec, Docker, Podman, Kubernetes (kubectl exec), and Custom (caller-supplied argv) all share the same `SpawnSpec::Bootstrap` path. Docker and Podman additionally support an opt-in `bootstrapless` (`docker cp` / `podman cp`) path for distroless / sh-less containers. Remaining:
- Host-key prompts and "are you sure you want to add fingerprint" flows for SSH still ride the existing askpass channel; verify the same UX with `StrictHostKeyChecking=accept-new` setups.
- The bootstrapless flow trusts the daemon's reported OS/arch from `inspect`. If the container runs a foreign-arch userspace under qemu the agent will be the wrong arch; consider a `--print-triple` self-check post-launch.
- Kubernetes is bootstrap-only (kubectl cp itself needs tar). If we want sh-less k8s pods later we'd need to teach the agent how to be `tar`-injected.

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Persist column widths across sessions (today they only persist for the lifetime of the session)
- Implement `Vfs::revalidate` for archive VFSes (zip + tar). Trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the central directory / entry index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid. Don't forget to flip `can_revalidate` to true on the descriptors.

## Compute dir sizes recursively (with caching)

Cache invalidation is the hard problem — filesystem events don't bubble up from subdirectories reliably.
