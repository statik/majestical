// The Captions section's words, pulled out of `CaptionsSection.svelte` the
// way `scheduler-status.ts` holds Always-on's: `KEY_STATUS` being a
// `Record<KeySource, string>` fails to compile if a key source is left out.
// Every string here is the approved mockup's
// (`docs/superpowers/specs/mockups/2026-09-18-phase7g/captions-section.html`).
import type { DescriberBackend, DescriberProbeOutcome, KeySource } from "./api-captions";

/** Each `baseUrl` must equal `BackendKind::default_base_url()`
 *  (`crates/describe/src/config.rs:37`). `captions-status.test.ts` pins
 *  OpenRouter's against the Rust-generated `describer_settings.json`; the
 *  two local ones are checked by eye against that function. */
export const BACKENDS: { value: DescriberBackend; label: string; baseUrl: string }[] = [
  { value: "ollama", label: "Ollama", baseUrl: "http://localhost:11434" },
  { value: "lm-studio", label: "LM Studio", baseUrl: "http://localhost:1234" },
  { value: "open-router", label: "OpenRouter", baseUrl: "https://openrouter.ai/api" },
];

export const UNCONFIGURED_LINE = "No describer is configured — captions are off.";
export const SAVED_LINE = "Saved.";

const KEY_STATUS: Record<KeySource, string> = {
  keychain: "A key is stored in the macOS Keychain.",
  env: "MAJ_OPENROUTER_KEY supplies the key and overrides a stored one.",
  file: "A key is stored in describer.toml. Save a key here to move it to the Keychain.",
  none: "No key. Captions cannot run until one is saved.",
};

export function keyStatusLine(source: KeySource): string {
  return KEY_STATUS[source];
}

/** Only a stored key can be removed: the app cannot unset an env var. */
export function removeKeyVisible(source: KeySource): boolean {
  return source === "keychain" || source === "file";
}

export function keyPlaceholder(source: KeySource): string {
  return removeKeyVisible(source) ? "Leave empty to keep the stored key" : "sk-or-…";
}

export interface TestLine {
  good: boolean;
  text: string;
}

function modelLine(probe: DescriberProbeOutcome): TestLine {
  return probe.model_listed
    ? { good: true, text: `Model ${probe.model} is listed.` }
    : { good: false, text: `Model ${probe.model} is not listed — check the name.` };
}

export function testLines(probe: DescriberProbeOutcome): TestLine[] {
  const lines: TestLine[] = [{ good: true, text: "Backend reachable." }, modelLine(probe)];
  if (probe.vision === true) lines.push({ good: true, text: "Vision: yes" });
  if (probe.vision === false) {
    lines.push({ good: false, text: "Vision: no — captions will not run with this model" });
  }
  if (probe.key === "accepted") lines.push({ good: true, text: "Key accepted." });
  if (probe.key === "rejected") {
    lines.push({
      good: false,
      text: "Key rejected — OpenRouter answered 401. Save a new key.",
    });
  }
  if (probe.key === "missing") lines.push({ good: false, text: keyStatusLine("none") });
  return lines;
}
