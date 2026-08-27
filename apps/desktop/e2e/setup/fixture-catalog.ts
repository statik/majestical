// Seeds a throwaway catalog the smoke spec points the real app at, by
// shelling out to the debug `maj` binary — the same binary the CLI's own
// integration tests drive (see crates/cli/tests/common/mod.rs's
// `fixture_catalog`, whose env vars and verb spellings this mirrors).
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

/**
 * `onPrepare` (this module's caller) runs in wdio's launcher process; the
 * spec runs in a separate worker process. A fixed path under the OS temp
 * dir is the handoff between them — simpler and more robust than relying
 * on the embedded WebDriver server to echo back a custom capability.
 */
const FIXTURE_INFO_PATH = path.join(tmpdir(), "maj-e2e-fixture.json");

/** `--volume` becomes both the volume's id and its label (see
 *  `resolve_volume` in crates/services/src/scan.rs), so this is what the
 *  Volumes table and the Browse tree's root node both show. */
const VOLUME_LABEL = "e2e-fixtures";
const TAG_NAME = "e2e-smoke";
const MACHINE_ID = "e2e-fixture";
const PHOTO_NAME = "fixture-photo";

export interface FixtureCatalog {
  /** The GUI config dir to point `MAJ_DESKTOP_CONFIG_DIR` at. */
  configDir: string;
  /** Passed to the app as `MAJ_STATE_DIR`, same value the fixture was
   *  seeded under — not required for correctness (the sqlite projection is
   *  disposable and rebuilds from the catalog's event log either way), but
   *  reusing it skips a redundant rebuild on first launch. */
  stateDir: string;
  volumeLabel: string;
  tagName: string;
  photoName: string;
}

function runMaj(bin: string, args: string[], env: NodeJS.ProcessEnv): string {
  const result = spawnSync(bin, args, { env, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(
      `maj ${args.join(" ")} failed (exit ${String(result.status)}):\n${result.stderr}`,
    );
  }
  return result.stdout;
}

/** The asset id `search --json` reports for the first hit — mirrors
 *  `first_asset_id` in crates/cli/tests/common/mod.rs. */
function firstAssetId(searchJson: string): string {
  const hits = JSON.parse(searchJson) as { results: { asset: string }[] };
  const first = hits.results[0];
  if (first === undefined) {
    throw new Error(`search returned no results: ${searchJson}`);
  }
  return first.asset;
}

/**
 * Builds a small fixture catalog (one scanned volume, three assets, one
 * tag) under a fresh temp directory, and writes the GUI's `config.json` so
 * pointing `MAJ_DESKTOP_CONFIG_DIR` at the returned `configDir` opens it.
 */
export async function setupFixtureCatalog(repoRoot: string): Promise<FixtureCatalog> {
  const majBin = path.join(repoRoot, "target/debug/maj");
  const mediaDir = path.resolve(import.meta.dirname, "../fixtures/media");

  const base = await mkdtemp(path.join(tmpdir(), "maj-e2e-"));
  const catalogDir = path.join(base, "catalog");
  const stateDir = path.join(base, "state");
  const configDir = path.join(base, "config");

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    MAJ_CATALOG: catalogDir,
    MAJ_MACHINE_ID: MACHINE_ID,
    MAJ_STATE_DIR: stateDir,
  };

  runMaj(majBin, ["catalog", "init"], env);
  runMaj(majBin, ["scan", mediaDir, "--volume", VOLUME_LABEL], env);
  const searchJson = runMaj(majBin, ["search", PHOTO_NAME, "--json"], env);
  const photoAssetId = firstAssetId(searchJson);
  runMaj(majBin, ["tag", "add", photoAssetId, TAG_NAME], env);

  await mkdir(configDir, { recursive: true });
  await writeFile(
    path.join(configDir, "config.json"),
    JSON.stringify({ catalog: catalogDir }),
  );

  const fixture: FixtureCatalog = {
    configDir,
    stateDir,
    volumeLabel: VOLUME_LABEL,
    tagName: TAG_NAME,
    photoName: PHOTO_NAME,
  };
  await writeFile(FIXTURE_INFO_PATH, JSON.stringify(fixture));
  return fixture;
}

/** Reads back what {@link setupFixtureCatalog} wrote, from the spec's own
 *  (worker) process. */
export async function readFixtureCatalog(): Promise<FixtureCatalog> {
  const text = await readFile(FIXTURE_INFO_PATH, "utf8");
  return JSON.parse(text) as FixtureCatalog;
}
