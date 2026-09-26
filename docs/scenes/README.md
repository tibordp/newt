# Documentation screenshots

Screenshots are rendered from canned **scenes** rather than taken by hand, so
they can be regenerated whenever the UI changes:

```sh
npx playwright install webkit   # once
npm run screenshots             # every scene, light and dark
npm run screenshots -- terminal editor --scheme dark
```

PNGs land in `docs/screenshots/` (`<scene>.png`, `<scene>-dark.png`; `--out`
to change) at 2×, in macOS window chrome with its shadow on a transparent
background. Run it on a Mac: default preferences, and with them the rendered
shortcuts, are exported from Rust for the host platform.

## Scenes

One file per scene in `library/`; the file name is the scene name. A scene is
the pushed state of one window — `MainWindowState`, `ViewerState` or
`EditorState` from `src/lib/bindings.ts` — so `tsc` flags any scene a Rust
state change leaves stale. On top of the state, a scene can carry:

- `files`: fixtures for the paths the window reads, keyed by `VfsPath.path`
  (`import x from "../fixtures/foo.rs?url"`). The viewer's file server is
  pointed at the fixture, so image, media and PDF modes work too.
- `terminals`: raw output per terminal handle, ANSI escapes and all.
- `preferences` / `runtimeState`: tweaks to the exported defaults.
- `commands`: extra IPC handlers, for dialogs that fetch their data.
- `steps`: Playwright `press` / `type` / `click` / `waitFor`, run before the
  capture — for state that lives in React, like a filtered command palette.
- `ready`: a selector that must be present before capture, for content that
  arrives by some route other than IPC.

`builders.ts` fills in the tedious parts of the state: listings with `..`,
sorting, stats and breadcrumbs, git annotations, operations.

To author a scene, run `npm run dev` and open
<http://localhost:1420/docs/scenes/frame.html>: it lists every scene and
renders the one picked, reloading as you edit. The page needs
`target/scenes/defaults.json`, which `npm run screenshots` writes.

## How it works

Nothing of the Rust side runs. `harness.ts` stands in for the backend with
Tauri's `mockIPC`: it answers `ping` with the scene state,
`get_preferences` / `get_runtime_state` with the defaults exported by
`newt --export-scene-defaults` (a `specta-bindings` build; locale pinned to
en-US), and file reads from the fixtures, then boots the real app on the
window's route inside `scene.html`. `frame.html` draws the window chrome
around that iframe.

The runner (`scripts/screenshots.mjs`) drives Playwright's WebKit, the
engine closest to WKWebView. It freezes the clock at 2026-07-28 17:35
(Europe/Ljubljana), disables animations, and captures once the app is idle:
state delivered, no IPC in flight, fonts and images loaded, and the ready
selectors present for two consecutive frames. An error dialog or a scene that
never settles fails that capture; commands without a mock are listed.

Transfer speed and ETA in the progress modal come from samples taken over
time, so a static scene shows neither.
