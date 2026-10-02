// The describers the Captions suites load, and the queries they read the
// form with. Shared by `CaptionsSection.test.ts` (the form and Save) and
// `CaptionsSection.actions.test.ts` (Test, Remove key, the in-flight
// guard) — one set of fixtures, so the two halves describe the same form.
import { screen, waitFor } from "@testing-library/svelte";
import { expect } from "vitest";
import type { DescriberProbeOutcome, DescriberSettingsOutcome, KeySource } from "./api-captions";
import describerProbe from "./fixtures/describer_probe.json";
import describerSettings from "./fixtures/describer_settings.json";
import describerSettingsUnconfigured from "./fixtures/describer_settings_unconfigured.json";
import { mockCommands } from "./test-support";

export const unconfigured = describerSettingsUnconfigured as DescriberSettingsOutcome;
/** OpenRouter with its key in the Keychain, and one notice. */
export const openRouter = describerSettings as DescriberSettingsOutcome;
export const probe = describerProbe as DescriberProbeOutcome;

export const ollama: DescriberSettingsOutcome = {
  describer: {
    backend: "ollama",
    base_url: "http://localhost:11434",
    model: "llava",
    key_source: "none",
  },
  keychain_supported: true,
};

export function withKeySource(source: KeySource): DescriberSettingsOutcome {
  const view = openRouter.describer;
  if (view === null) throw new Error("describer_settings.json must be configured");
  return { ...openRouter, describer: { ...view, key_source: source } };
}

/** Loads `openRouter` and records every `save_describer` call's `req`. */
export function recordSaves(answer: () => unknown): { reqs: Record<string, unknown>[] } {
  const reqs: Record<string, unknown>[] = [];
  mockCommands({
    describer_settings: () => openRouter,
    save_describer: (args) => {
      reqs.push((args as { req: Record<string, unknown> }).req);
      return answer();
    },
  });
  return { reqs };
}

export function button(name: string): HTMLButtonElement {
  return screen.getByRole("button", { name }) as HTMLButtonElement;
}

export function field(name: string): HTMLInputElement {
  return screen.getByLabelText(name) as HTMLInputElement;
}

export function backendSelect(): HTMLSelectElement {
  return screen.getByLabelText("Backend") as HTMLSelectElement;
}

/** Waits until the form holds the loaded describer's model. */
export async function loaded(model: string): Promise<void> {
  await waitFor(() => expect(field("Model").value).toBe(model));
}
