import { randomUUID } from "node:crypto";
import { readdir, rm } from "node:fs/promises";
import path from "node:path";
import { SevereServiceError } from "webdriverio";
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

// The app under test and every `maj` the fixture setup runs would otherwise
// read and write the developer's REAL login-Keychain item. Each run gets its
// own throwaway service instead, under the same `majestical-test-` prefix the
// Rust test guards insist on (crates/secrets/src/system.rs). The tauri-service
// spawns the app with `{ ...process.env, ...options.env }` from this launcher
// process, so an ambient key can only be kept out by deleting it here.
const KEYCHAIN_SERVICE_ENV = "MAJ_KEYCHAIN_SERVICE";
const OPENROUTER_KEY_ENV = "MAJ_OPENROUTER_KEY";
const THROWAWAY_KEYCHAIN_PREFIX = "majestical-test-";

/** Points this launcher (and so the app and the fixture's `maj` children
 *  it spawns) at a throwaway Keychain service and drops an inherited
 *  OpenRouter key. */
function isolateKeychain(): string {
  const service = `${THROWAWAY_KEYCHAIN_PREFIX}e2e-${String(process.pid)}-${randomUUID()}`;
  process.env[KEYCHAIN_SERVICE_ENV] = service;
  delete process.env[OPENROUTER_KEY_ENV];
  return service;
}

/**
 * The backstop. `prepare` runs it on the env the fixture's `maj` children
 * will get, then — as its last step, before the tauri-service spawns the
 * app — on the env the app WILL get (`{ ...process.env, ...capability env
 * }`). It checks the result, not what `isolateKeychain` meant to set, so
 * losing the isolation call fails the run.
 * No later hook can do this job: the service spawns the app in its own
 * `onPrepare`, right after this one. What stays unguarded is a change that
 * deletes these calls too — a TypeScript harness has no equivalent of
 * crates/cli/tests/keychain_guard.rs scanning it, so review is the limit.
 */
function assertKeychainIsolated(env: Record<string, string | undefined>): void {
  const service = env[KEYCHAIN_SERVICE_ENV];
  if (service === undefined || !service.startsWith(THROWAWAY_KEYCHAIN_PREFIX)) {
    throw new Error(
      `refusing to launch: ${KEYCHAIN_SERVICE_ENV} is ${String(service)}, ` +
        `not a ${THROWAWAY_KEYCHAIN_PREFIX}* throwaway service`,
    );
  }
  if (env[OPENROUTER_KEY_ENV] !== undefined) {
    throw new Error(`refusing to launch: ${OPENROUTER_KEY_ENV} would be inherited`);
  }
}

// Explicit order, ingest LAST: an ingest run appends immutable events (two
// new assets, a new volume for the destination) that `volumes.e2e.ts`'s
// exact-count asserts would see; nothing can undo them, so nothing runs
// after it. Every spec file must be listed here (a new one goes before
// ingest); `onPrepare` stops every run, single-spec ones included, when the
// files on disk and this list differ, since the old glob would have picked
// a new file up silently and this list would not. A module const, not read
// back from the config: the launcher rewrites `config.specs` with a `--spec`
// filter before `onPrepare` runs, and a single-spec debugging run must keep
// working.
const SPEC_FILES = [
  "./specs/smoke.e2e.ts",
  "./specs/search.e2e.ts",
  "./specs/volumes.e2e.ts",
  "./specs/browse.e2e.ts",
  "./specs/organize.e2e.ts",
  "./specs/settings.e2e.ts",
  "./specs/ingest.e2e.ts",
];

/** Fails when the spec files on disk and `SPEC_FILES` differ. */
async function assertSpecListComplete(): Promise<void> {
  const onDisk = (await readdir(path.join(import.meta.dirname, "specs"))).filter((file) =>
    file.endsWith(".e2e.ts"),
  );
  const listed = SPEC_FILES.map((spec) => path.basename(spec));
  const same = onDisk.length === listed.length && onDisk.every((file) => listed.includes(file));
  if (!same) {
    throw new Error(
      `spec files on disk [${onDisk.join(", ")}] differ from wdio.conf.ts's list [${listed.join(", ")}]`,
    );
  }
}

/**
 * The launcher's setup, before any worker (and so the app under test)
 * spawns. Keychain isolation comes first, ahead of anything that can spawn
 * `maj` or fail. Seeds the fixture catalog once, then hands the app its
 * `MAJ_DESKTOP_CONFIG_DIR`/`MAJ_STATE_DIR`/`MAJ_KEYCHAIN_SERVICE` via the
 * tauri-service's per-capability `env` override, and hands the fixture's
 * own details to the spec via `FIXTURE_ENV_VAR` (the local runner's workers
 * inherit the launcher's env, so this needs no file or capability
 * round-trip).
 */
async function prepare(capabilities: TauriCapability[]): Promise<void> {
  const keychainService = isolateKeychain();
  assertKeychainIsolated(process.env);
  await assertSpecListComplete();
  const fixture = await setupFixtureCatalog(repoRoot);
  process.env[FIXTURE_ENV_VAR] = JSON.stringify(fixture);

  const [capability] = capabilities;
  if (capability === undefined) {
    throw new Error("expected exactly one capability");
  }
  capability["wdio:tauriServiceOptions"] = {
    env: {
      MAJ_DESKTOP_CONFIG_DIR: fixture.configDir,
      MAJ_STATE_DIR: fixture.stateDir,
      [KEYCHAIN_SERVICE_ENV]: keychainService,
    },
  };
  assertKeychainIsolated({ ...process.env, ...capability["wdio:tauriServiceOptions"].env });
}

export const config: Config = {
  runner: "local",
  specs: SPEC_FILES,
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
  // WDIO 9's `runLauncherHook` (@wdio/cli) logs a plain Error thrown from
  // this hook and carries on into the tauri-service's own `onPrepare`, which
  // spawns the app; only a `SevereServiceError` stops the run. So every
  // refusal in `prepare` is rethrown as one.
  onPrepare: async (_wdioConfig, capabilities) => {
    try {
      await prepare(capabilities as TauriCapability[]);
    } catch (error) {
      if (error instanceof SevereServiceError) throw error;
      // The constructor takes only a message; keep the original for its stack.
      const severe = new SevereServiceError(error instanceof Error ? error.message : String(error));
      severe.cause = error;
      throw severe;
    }
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
