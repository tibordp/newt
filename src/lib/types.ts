/// Re-exports of types that flow over IPC. The canonical definitions live in
/// `bindings.ts` (generated from Rust by tauri-specta); this module exists so
/// non-IPC code can import them without coupling to the generated path.
export type {
  Breadcrumb,
  HistoryEntryView,
  HotPathCategory,
  HotPathEntry,
  VfsPath,
  VfsTarget,
} from "./bindings";
