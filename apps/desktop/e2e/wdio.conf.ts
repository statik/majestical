import { rm } from "node:fs/promises";
import path from "node:path";
import {
  FIXTURE_ENV_VAR,
  setupFixtureCatalog,
} from "./setup/fixture-catalog.ts";

const repoRoot = path.resolve(import.meta.dirname, "../../..");

// `tauri build --debug` (the "gui" job's release job builds --release; this
// suite never touches that path) bundles the frontend + the debug binary
// into a `.app`, per Tauri's default macOS bundle layout. The tauri-service
// spawns this path directly (no `open -a`), so it must be the executable
// inside the bundle — pointing it at the `.app` directory itself fails
// with EACCES.
const appBundlePath = path.join(
  repoRoot,
  "apps/desktop/src-tauri/target/debug/bundle/macos/Majestical.app/Contents/MacOS/majestical-desktop",
);

/**
 * `@wdio/tauri-service` doesn't augment `WebdriverIO.Capabilities` with its
 * own vendor keys, so a plain `WebdriverIO.Config` rejects `tauri:options`
 * as an excess property. This narrows just the `capabilities` field to a
 * shape that includes what the service actually reads.
 */
type TauriCapability = WebdriverIO.Capabilities & {
  "tauri:options"?: { application: string };
  "wdio:tauriServiceOptions"?: { env: Record<string, string> };
};

type Config = Omit<WebdriverIO.Config, "capabilities"> & {
  capabilities: TauriCapability[];
};

export const config: Config = {
  runner: "local",
  specs: ["./specs/**/*.e2e.ts"],
  maxInstances: 1,
  logLevel: "info",
  bail: 0,
  waitforTimeout: 10_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 3,
  framework: "mocha",
  mochaOpts: {
    ui: "bdd",
    timeout: 60_000,
  },
  reporters: ["spec"],
  // No `browser.tauri.*` bridge: the smoke spec only needs plain WebDriver
  // element queries (getTitle, $, click, getText), which work with just
  // tauri-plugin-wdio-webdriver's embedded server — no `@wdio/tauri-plugin`
  // frontend import, and no `withGlobalTauri` overlay config. Going without
  // it does mean the service's own internal window-focus recovery (which
  // needs the bridge) can't work either — the spec's `before()` hook
  // suppresses that check the standard way; see its comment. The
  // `--config src-tauri/tauri.e2e.conf.json` this suite's build commands
  // pass exists for an unrelated reason: it turns off
  // `createUpdaterArtifacts`, which otherwise makes `tauri build` exit
  // non-zero trying to sign the updater tarball with a private key no
  // debug/CI build has. See the phase 7E task 4 report for how the
  // no-bridge call was verified.
  services: [
    [
      "@wdio/tauri-service",
      {
        appBinaryPath: appBundlePath,
        driverProvider: "embedded",
        startTimeout: 90_000,
      },
    ],
  ],
  capabilities: [
    {
      browserName: "tauri",
      "tauri:options": {
        application: appBundlePath,
      },
    },
  ],
  // Seeds the fixture catalog once, in the launcher process, before any
  // worker (and so the app under test) spawns — then hands the app its
  // `MAJ_DESKTOP_CONFIG_DIR`/`MAJ_STATE_DIR` via the tauri-service's
  // per-capability `env` override, and hands the fixture's own details to
  // the spec via `FIXTURE_ENV_VAR` (the local runner's workers inherit the
  // launcher's env, so this needs no file or capability round-trip).
  onPrepare: async (_wdioConfig, capabilities) => {
    const fixture = await setupFixtureCatalog(repoRoot);
    process.env[FIXTURE_ENV_VAR] = JSON.stringify(fixture);

    const [capability] = capabilities as TauriCapability[];
    if (capability === undefined) {
      throw new Error("expected exactly one capability");
    }
    capability["wdio:tauriServiceOptions"] = {
      env: {
        MAJ_DESKTOP_CONFIG_DIR: fixture.configDir,
        MAJ_STATE_DIR: fixture.stateDir,
      },
    };
  },
  // Removes the fixture's mkdtemp tree (catalog, state dir, GUI config) —
  // by now the service has already torn down the app and its embedded
  // driver, so nothing still holds these files open.
  onComplete: async () => {
    const raw = process.env[FIXTURE_ENV_VAR];
    if (raw === undefined) return;
    const fixture = JSON.parse(raw) as { configDir: string };
    await rm(path.dirname(fixture.configDir), { recursive: true, force: true });
  },
};
