import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  // prevent vite from obscuring rust errors
  clearScreen: false,
  // tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
  },
  // to make use of `TAURI_DEBUG` and other env variables
  // https://tauri.studio/v1/api/config#buildconfig.beforedevcommand
  envPrefix: ["VITE_", "TAURI_"],
  // Build-time platform literal. Tauri v2 sets TAURI_ENV_PLATFORM when it
  // invokes the frontend build/dev command (v1 used TAURI_PLATFORM); accept
  // either. Folds to a constant so Windows-only code (the WSL picker) is
  // tree-shaken out of non-Windows bundles.
  define: {
    __WINDOWS__: JSON.stringify(
      process.env.TAURI_ENV_PLATFORM === "windows" ||
        process.env.TAURI_PLATFORM === "windows",
    ),
  },
  build: {
    // Tauri supports es2021
    target: process.env.TAURI_PLATFORM == "windows" ? "chrome105" : "safari13",
    // don't minify for debug builds
    minify: !process.env.TAURI_DEBUG ? true : false,
    // produce sourcemaps for debug builds
    sourcemap: !!process.env.TAURI_DEBUG,
  },
}));
