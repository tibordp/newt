# TODO

## Askpass support for SFTP VFS

## Archive VFS

Read-only (initially) VFS for browsing archive contents (tar, zip, etc.) as a mounted filesystem. Major feature, probably its own crate for the juicy part, which is
world-class TAR support (with random access - idea is the same as ratarmount, we do one pass through the tar, build an index of file -> offset in decompressed stream plus
entire decompressor state at various checkpoints spaced ~every 10MB or so). This will allow us to efficiently do random byte range requests even through remoting without
having to unpack everything.

## Recursive file search

## Archive packing and unpacking

(as an operation, not a VFS)

## Bug fixes and strengthening

- S3 connect dialog with the ability to pick profile or enter credentials manually.
- SFTP dialog not focusing when selected from the VFS dropdown
- Ability to unmount VFSes

## Customizability

