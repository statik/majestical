import { expect, test } from "vitest";
import type { DescriberProbeOutcome, DescriberSettingsOutcome } from "./api-captions";
import {
  BACKENDS,
  keyPlaceholder,
  keyStatusLine,
  removeKeyVisible,
  testLines,
} from "./captions-status";
import describerSettings from "./fixtures/describer_settings.json";

const configured = describerSettings as DescriberSettingsOutcome;

const probe: DescriberProbeOutcome = {
  model: "google/gemini-2.5-flash",
  model_listed: true,
  vision: null,
  key: "accepted",
};

test("OpenRouter's default base URL is the one the Rust side reports", () => {
  const openRouter = BACKENDS.find((backend) => backend.value === "open-router");
  expect(configured.describer?.backend).toBe("open-router");
  expect(openRouter?.baseUrl).toBe(configured.describer?.base_url);
});

test("keyStatusLine reads each source's line", () => {
  expect(keyStatusLine("keychain")).toBe("A key is stored in the macOS Keychain.");
  expect(keyStatusLine("env")).toBe(
    "MAJ_OPENROUTER_KEY supplies the key and overrides a stored one.",
  );
  expect(keyStatusLine("file")).toBe(
    "A key is stored in describer.toml. Save a key here to move it to the Keychain.",
  );
  expect(keyStatusLine("none")).toBe("No key. Captions cannot run until one is saved.");
});

test("only a stored key can be removed", () => {
  expect(removeKeyVisible("keychain")).toBe(true);
  expect(removeKeyVisible("file")).toBe(true);
  expect(removeKeyVisible("env")).toBe(false);
  expect(removeKeyVisible("none")).toBe(false);
});

test("keyPlaceholder offers to keep a stored key, and shows the key's shape otherwise", () => {
  expect(keyPlaceholder("keychain")).toBe("Leave empty to keep the stored key");
  expect(keyPlaceholder("none")).toBe("sk-or-…");
});

test("testLines for an all-good OpenRouter probe", () => {
  expect(testLines(probe)).toEqual([
    { good: true, text: "Backend reachable." },
    { good: true, text: "Model google/gemini-2.5-flash is listed." },
    { good: true, text: "Key accepted." },
  ]);
});

test("testLines names an unlisted model and a rejected key", () => {
  expect(
    testLines({ ...probe, model: "gemini-flash", model_listed: false, key: "rejected" }),
  ).toEqual([
    { good: true, text: "Backend reachable." },
    { good: false, text: "Model gemini-flash is not listed — check the name." },
    { good: false, text: "Key rejected — OpenRouter answered 401. Save a new key." },
  ]);
});

test("testLines for a missing key reads the no-key status line", () => {
  expect(testLines({ ...probe, key: "missing" })[2]).toEqual({
    good: false,
    text: keyStatusLine("none"),
  });
});

test("testLines for LM Studio with a vision model", () => {
  expect(testLines({ ...probe, model: "llava", vision: true, key: "not_checked" })).toEqual([
    { good: true, text: "Backend reachable." },
    { good: true, text: "Model llava is listed." },
    { good: true, text: "Vision: yes" },
  ]);
});

test("testLines for LM Studio with a model that cannot see", () => {
  expect(testLines({ ...probe, model: "qwen", vision: false, key: "not_checked" })).toEqual([
    { good: true, text: "Backend reachable." },
    { good: true, text: "Model qwen is listed." },
    { good: false, text: "Vision: no — captions will not run with this model" },
  ]);
});

test("testLines for Ollama is exactly reachability and the model", () => {
  expect(testLines({ ...probe, model: "llava", vision: null, key: "not_checked" })).toEqual([
    { good: true, text: "Backend reachable." },
    { good: true, text: "Model llava is listed." },
  ]);
});
