import { defineConfig } from "vite";

export default defineConfig({
  root: "frontend",
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "0.0.0.0", hmr: process.env.TAURI_DEV_HOST ? { host: process.env.TAURI_DEV_HOST, protocol: "ws", port: 1421 } : undefined, watch: { ignored: ["**/src-tauri/**"] } },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: { target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13", outDir: "dist", emptyOutDir: true }
});
