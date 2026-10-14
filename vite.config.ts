import { defineConfig } from "vite";

// Tauri expects a fixed dev port and must not have the screen cleared.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
});
