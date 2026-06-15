/// <reference types="vite/client" />

// Build-time constant injected by Vite `define` (see vite.config.ts);
// `true` only for Windows builds. Guards Windows-only UI so it is
// dead-code-eliminated elsewhere.
declare const __WINDOWS__: boolean;
