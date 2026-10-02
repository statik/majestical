// The captions wire subject — `captions.rs`'s four commands. Its own
// module because `api.ts` is at its cap (see .oxlintrc.json); imported
// directly by `CaptionsSection.svelte`, not spread into `api`. Same rules
// as api.ts: one interface per outcome struct, mirroring the Rust
// field-for-field, pinned by `fixtures.captions.test.ts` against
// `fixtures/*.json`.
import { invoke } from "@tauri-apps/api/core";

/** `describer_config::DescriberBackend`, serialized kebab-case. */
export type DescriberBackend = "ollama" | "lm-studio" | "open-router";

/** `describer_config::KeySource`, serialized snake_case. */
export type KeySource = "env" | "keychain" | "file" | "none";

/** `describer_config::DescriberConfigView`. Never carries a key. */
export interface DescriberConfigView {
  backend: DescriberBackend;
  base_url: string;
  model: string;
  key_source: KeySource;
}

/** `captions::DescriberSettingsOutcome`. `describer` is `null`, not absent,
 *  when nothing is configured. */
export interface DescriberSettingsOutcome {
  describer: DescriberConfigView | null;
  keychain_supported: boolean;
  notices?: string[];
}

/** `describer_config::KeyCheck`, serialized snake_case. */
export type KeyCheck = "accepted" | "rejected" | "missing" | "not_checked";

/** `captions::DescriberProbeOutcome` — the probe, flattened, plus notices.
 *  `vision` is `null` for every backend but LM Studio. */
export interface DescriberProbeOutcome {
  model: string;
  model_listed: boolean;
  vision: boolean | null;
  key: KeyCheck;
  notices?: string[];
}

/** `captions::SaveDescriberReq`. An absent or blank `api_key` keeps the
 *  stored key; an absent or blank `base_url` means the backend's default. */
export interface SaveDescriberReq {
  backend: DescriberBackend;
  model: string;
  base_url?: string;
  api_key?: string;
}

export const captionsApi = {
  describerSettings: () => invoke<DescriberSettingsOutcome>("describer_settings"),
  saveDescriber: (req: SaveDescriberReq) =>
    invoke<DescriberSettingsOutcome>("save_describer", { req }),
  clearDescriberKey: () => invoke<DescriberSettingsOutcome>("clear_describer_key"),
  testDescriber: () => invoke<DescriberProbeOutcome>("test_describer"),
};
