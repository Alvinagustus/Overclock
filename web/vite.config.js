import { defineConfig } from "vite";

export default defineConfig({
  define: {
    global: "globalThis",
    "process.env": {},
  },
  optimizeDeps: {
    esbuildOptions: {
      define: {
        global: "globalThis",
        "process.env": "{}",
      },
    },
  },
  server: { port: 5174 },
});
