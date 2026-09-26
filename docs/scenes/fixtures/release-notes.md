# Newt 0.9 — release notes

## Highlights

- **Flat S3 listings.** Browsing a bucket no longer walks every prefix up
  front; listings stream in as pages arrive and the status bar says when a
  result is partial.
- **Exclusive writes.** Copy and move ask the destination to refuse an
  existing file instead of racing a separate stat, so a conflict found
  mid-transfer is reported, never silently overwritten.
- **Up-front conflict answers.** Choose "overwrite older", "skip" or "keep
  both" before the operation starts, and it never has to stop and ask.

## Viewer

- Text mode detects the encoding from the first 64 KiB and shows it in the
  status bar; pick another from the Encoding menu.
- Hex mode copies selections as hex, C array, or Base64.
- Images show EXIF metadata in a side panel (I).

## Fixes

- Delete and recursive attribute changes stop at mount points.
- Walks report the directories they could not read instead of failing
  the whole operation.
- The terminal keeps its scrollback across panel toggles.

## Upgrading

Settings files from 0.8 load unchanged. Keybindings removed in this release
are listed in `settings.toml` under `[bindings]` with `command = "-"` so you
can restore them.
