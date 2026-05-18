/// <reference types="vite/client" />

// Build-time platform constant injected by Vite `define` (see
// vite.config.ts). `true` only when Tauri builds the frontend for Windows.
// Guards Windows-only UI (WSL) so it is dead-code-eliminated elsewhere.
declare const __WINDOWS__: boolean;
