// Vite config for the Aperture overlay (doc 11 §2). Tauri v2 loads this dev
// server in the transparent always-on-top WebView2 window. `clearScreen: false`
// keeps the Rust `cargo tauri dev` logs visible alongside Vite's output.
//
// [VERIFY] vite + plugin versions and the Tauri dev-server contract at build time.

import { defineConfig } from "vite"; // [VERIFY]
import react from "@vitejs/plugin-react"; // [VERIFY]

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  // Tauri expects a fixed port; fail fast rather than silently picking another.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },

  // Produce relative asset URLs so the bundle loads from the tauri:// scheme.
  base: "./",

  build: {
    // WebView2 is evergreen Chromium; target a recent baseline.
    target: "es2021",
    // Overlay is tiny; keep sourcemaps for debugging the compositor path.
    sourcemap: true,
  },
});
