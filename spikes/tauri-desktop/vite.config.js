import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    watch: {
      ignored: [
        "**/e2e/workdir/**",
        "**/e2e/artifacts/**",
        "**/src-tauri/target/**"
      ]
    }
  }
});
