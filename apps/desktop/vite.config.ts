import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";
import { defineConfig } from "vitest/config";

// `svelteTesting()` is inert outside vitest. It puts `browser` ahead of `node`
// in resolve.conditions so Svelte's client build (the one with `mount`) wins
// over its SSR build, and registers testing-library's per-test DOM cleanup.
export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // `css` defaults to false, which hands every CSS import back as an empty
  // string — `styles.test.ts` asserts on computed layout and needs the real
  // sheet in the document, so it is processed and injected for real.
  test: { environment: "jsdom", css: true },
});
