/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig(({ mode }) => {
  // Reads VITE_PROXY_TARGET from the environment or .env files.
  const env = loadEnv(mode, ".", "VITE_");
  const proxyTarget = env.VITE_PROXY_TARGET ?? "http://127.0.0.1:4500";

  return {
    base: "/",
    plugins: [react(), tailwindcss()],
    server: {
      proxy: {
        "/api": proxyTarget,
        "/livez": proxyTarget,
      },
    },
    build: {
      outDir: "dist",
    },
    test: {
      coverage: {
        provider: "v8",
        reporter: ["json"],
        reportOnFailure: true,
        include: ["src/**/*.{ts,tsx}"],
        exclude: [
          "src/**/*.test.ts",
          "src/**/*.test.tsx",
          "src/types/generated/**",
        ],
      },
    },
  };
});
