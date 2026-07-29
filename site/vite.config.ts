import { defineConfig } from "vite";

export default defineConfig({
  base: "/majestical/",
  build: { target: "es2022", sourcemap: false },
});
