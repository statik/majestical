// Seeds a throwaway catalog the smoke spec points the real app at, by
// shelling out to the debug `maj` binary — the same binary the CLI's own
// integration tests drive (see crates/cli/tests/common/mod.rs's
// `fixture_catalog`, whose env vars and verb spellings this mirrors).
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

/** `--volume` becomes both the volume's id and its label (see
 *  `resolve_volume` in crates/services/src/scan.rs), so this is what the
 *  Volumes table and the Browse tree's root node both show. */
const VOLUME_LABEL = "e2e-fixtures";
const TAG_NAME = "e2e-smoke";
const MACHINE_ID = "e2e-fixture";
const PHOTO_NAME = "fixture-photo";

/** The env var `onPrepare` (launcher process) stores the fixture under, and
 *  the spec (worker process) reads it back from — the wdio local runner
 *  spawns workers inheriting the launcher's env, so this needs no file or
 *  capability round-trip. */
export const FIXTURE_ENV_VAR = "MAJ_E2E_FIXTURE";

export interface FixtureCatalog {
  /** The GUI config dir to point `MAJ_DESKTOP_CONFIG_DIR` at. */
  configDir: string;
  /** Passed to the app as `MAJ_STATE_DIR`, same value the fixture was
   *  seeded under — not required for correctness (the sqlite projection is
   *  disposable and rebuilds from the catalog's event log either way), but
   *  reusing it skips a redundant rebuild on first launch. */
  stateDir: string;
  /** The catalog's own event-log directory (what `config.json`'s `catalog`
   *  field also points at) — exposed so a spec that mutates the catalog
   *  through the GUI (organize.e2e.ts) can shell back out to `maj` to undo
   *  it afterward. `onPrepare` seeds this catalog once for the whole `wdio
   *  run`, not once per spec file, so a mutation left standing here would
   *  leak into every file that runs after it in the same invocation. */
  catalogDir: string;
  volumeLabel: string;
  tagName: string;
  /** The tagged fixture asset's on-disk name (`PHOTO_NAME` + its real
   *  extension) — what `SearchHit.name`/`BrowseVolume`'s listing report for
   *  it, and so what the search and browse specs assert cards show. */
  photoFileName: string;
}

export function runMaj(bin: string, args: string[], env: NodeJS.ProcessEnv): string {
  const result = spawnSync(bin, args, { env, encoding: "utf8" });
  if (result.error) {
    throw new Error(
      `could not run ${bin} (${result.error.message}) — build it with \`cargo build -p majestical-cli\``,
    );
  }
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

  return {
    configDir,
    stateDir,
    catalogDir,
    volumeLabel: VOLUME_LABEL,
    tagName: TAG_NAME,
    photoFileName: `${PHOTO_NAME}.jpg`,
  };
}

/** Reads back what `onPrepare` stored at `process.env[FIXTURE_ENV_VAR]`. */
export function readFixtureCatalog(): FixtureCatalog {
  const raw = process.env[FIXTURE_ENV_VAR];
  if (raw === undefined) {
    throw new Error(`${FIXTURE_ENV_VAR} is not set — onPrepare must run before this reads it`);
  }
  return JSON.parse(raw) as FixtureCatalog;
}

/** The env every `maj` call against this fixture's catalog needs — the same
 *  three vars `setupFixtureCatalog` built by hand, reusable by a spec that
 *  needs its own follow-up call rather than the one-shot setup pass. */
function fixtureEnv(fixture: FixtureCatalog): NodeJS.ProcessEnv {
  return {
    ...process.env,
    MAJ_CATALOG: fixture.catalogDir,
    MAJ_MACHINE_ID: MACHINE_ID,
    MAJ_STATE_DIR: fixture.stateDir,
  };
}

/**
 * Removes `tag` from every asset that carries it. For a spec that assigns a
 * tag through the GUI as its own mutation (organize.e2e.ts) and then needs
 * the shared catalog back the way every other spec file in the same `wdio
 * run` still assumes it — this fixture is seeded once in `onPrepare`, not
 * once per file, so a tag left standing here is visible to every spec that
 * runs after it.
 */
export function removeTag(repoRoot: string, fixture: FixtureCatalog, tag: string): void {
  const majBin = path.join(repoRoot, "target/debug/maj");
  const env = fixtureEnv(fixture);
  const searchJson = runMaj(majBin, ["search", `tag:${tag}`, "--json"], env);
  const hits = JSON.parse(searchJson) as { results: { asset: string }[] };
  for (const hit of hits.results) {
    runMaj(majBin, ["tag", "rm", hit.asset, tag], env);
  }
}
