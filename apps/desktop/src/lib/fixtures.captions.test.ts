import { describe, expect, it } from "vitest";
import type {
  DescriberBackend,
  DescriberProbeOutcome,
  DescriberSettingsOutcome,
  KeyCheck,
  KeySource,
} from "./api-captions";
import describerProbe from "./fixtures/describer_probe.json";
import describerSettings from "./fixtures/describer_settings.json";
import describerSettingsUnconfigured from "./fixtures/describer_settings_unconfigured.json";

// Key sets are compared as `Set`s: a field Rust adds or drops fails here
// even when the TS interface would let it through.
//
// `backend`, `key_source` and `key` are string-literal unions; JSON module
// inference widens them to `string`, same reason `AssetDetail` in
// `fixtures.test.ts` needs a cast rather than a plain assignment. The
// annotated literal arrays below are the check the cast would otherwise
// hide: a value a union drops fails `tsc` there.
const typedSettings: DescriberSettingsOutcome =
  describerSettings as DescriberSettingsOutcome;
const typedUnconfigured: DescriberSettingsOutcome =
  describerSettingsUnconfigured as DescriberSettingsOutcome;
const typedProbe: DescriberProbeOutcome = describerProbe as DescriberProbeOutcome;
const allBackends: DescriberBackend[] = ["ollama", "lm-studio", "open-router"];
const allKeySources: KeySource[] = ["env", "keychain", "file", "none"];
const allKeyChecks: KeyCheck[] = ["accepted", "rejected", "missing", "not_checked"];

describe("describer settings fixtures", () => {
  it("carry a configured describer's view, never a key, and a notice", () => {
    expect(new Set(Object.keys(typedSettings))).toEqual(
      new Set([
        "describer",
        "keychain_supported",
        "notices",
      ]),
    );
    const view = typedSettings.describer;
    if (view === null) {
      throw new Error("the configured fixture must carry a describer");
    }
    expect(new Set(Object.keys(view))).toEqual(
      new Set([
        "backend",
        "base_url",
        "key_source",
        "model",
      ]),
    );
    expect(allBackends).toContain(view.backend);
    expect(view.backend).toBe("open-router");
    expect(allKeySources).toContain(view.key_source);
    expect(view.key_source).toBe("keychain");
    expect(typedSettings.keychain_supported).toBe(true);
    expect(typedSettings.notices?.length).toBeGreaterThan(0);
  });

  it("carry an unconfigured describer as null, not absent, with no notices", () => {
    expect(new Set(Object.keys(typedUnconfigured))).toEqual(
      new Set([
        "describer",
        "keychain_supported",
      ]),
    );
    expect(typedUnconfigured.describer).toBeNull();
    expect(typedUnconfigured.notices).toBeUndefined();
  });
});

describe("describer probe fixture", () => {
  it("flattens the probe beside its notices, with vision null off LM Studio", () => {
    expect(new Set(Object.keys(typedProbe))).toEqual(
      new Set([
        "key",
        "model",
        "model_listed",
        "notices",
        "vision",
      ]),
    );
    expect(typedProbe.vision).toBeNull();
    expect(allKeyChecks).toContain(typedProbe.key);
    expect(typedProbe.key).toBe("accepted");
    expect(typedProbe.model_listed).toBe(true);
    expect(typedProbe.notices?.length).toBeGreaterThan(0);
  });
});
