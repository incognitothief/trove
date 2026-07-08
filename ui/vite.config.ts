import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The UI is a pure presentation layer: it proxies /api to the local
// trove-serverd daemon and never talks to S3, SQLite, or volumes directly.
const SERVERD = process.env.TROVE_SERVERD_URL ?? "http://127.0.0.1:7377";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5273,
    proxy: {
      "/api": {
        target: SERVERD,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/api/, ""),
      },
    },
  },
});
