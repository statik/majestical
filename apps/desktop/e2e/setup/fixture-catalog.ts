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

/** The PARA node the Ingest spec files its run into, created here rather
 *  than through the GUI — the Ingest surface can only choose a node, and a
 *  catalog with none at all offers nothing to choose. `maj para add
 *  project <name>`, so the surface's `<select>` renders it as
 *  `project/e2e-ingest`. */
const PARA_NODE = "e2e-ingest";

/** What the two ingest source files hold. The bytes matter: `plan_source`
 *  (crates/ingest/src/plan.rs) decides `Copy` vs `Duplicate` by looking the
 *  file's SIZE up in the catalog first and only hashing on a size hit, so
 *  14-byte files in a catalog whose assets are 82, 365 and 2203 bytes are
 *  never even hashed — they plan as `Copy`, which is what the spec asserts.
 *  Non-empty also matters: a 0-byte file is `Rejected` outright. */
const INGEST_SOURCE_FILES: [string, string][] = [
  ["clip-a.txt", "alpha take one"],
  ["clip-b.txt", "bravo take two"],
];

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
  /** The debug `maj` binary this fixture was seeded with — so a spec that
   *  needs its own follow-up `maj` call (organize.e2e.ts's cleanup) doesn't
   *  re-derive the repo root and rebuild this path itself. */
  majBin: string;
  volumeLabel: string;
  tagName: string;
  /** The tagged fixture asset's on-disk name (`PHOTO_NAME` + its real
   *  extension) — what `SearchHit.name`/`BrowseVolume`'s listing report for
   *  it, and so what the search and browse specs assert cards show. */
  photoFileName: string;
  /** A folder holding two small files that are NOT in the catalog — what
   *  the Ingest spec types into the Source box. Deliberately outside the
   *  scanned `fixtures/media` tree so `plan_ingest` decides "copy" for both
   *  files rather than "duplicate". */
  ingestSourceDir: string;
  /** An empty folder the Ingest spec types into the Destinations box, and
   *  then reads back to prove the copy really landed. Empty at seed time,
   *  so anything found under it afterwards was placed by the run. */
  ingestDestDir: string;
  /** The `project` node seeded here for the Ingest spec to file its run
   *  into — the surface's `<select>` shows it as `project/<this>`. */
  paraNodeName: string;
}

function runMaj(bin: string, args: string[], env: NodeJS.ProcessEnv): string {
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
 * The Ingest spec's job: a source `setupFixtureCatalog`'s scan never saw, an empty
 * destination, and the PARA node to file into. Seeded alongside the catalog
 * rather than inside that spec so it lives and dies with the same mkdtemp
 * tree `onComplete` removes, and so the app finds the node already in the
 * catalog it opens.
 */
async function seedIngestJob(
  base: string,
  majBin: string,
  env: NodeJS.ProcessEnv,
): Promise<{ ingestSourceDir: string; ingestDestDir: string; paraNodeName: string }> {
  const ingestSourceDir = path.join(base, "ingest-src");
  const ingestDestDir = path.join(base, "ingest-dst");
  await mkdir(ingestSourceDir, { recursive: true });
  await mkdir(ingestDestDir, { recursive: true });
  for (const [name, contents] of INGEST_SOURCE_FILES) {
    await writeFile(path.join(ingestSourceDir, name), contents);
  }
  runMaj(majBin, ["para", "add", "project", PARA_NODE], env);
  return { ingestSourceDir, ingestDestDir, paraNodeName: PARA_NODE };
}

/**
 * Builds a small fixture catalog (one scanned volume, three assets, one
 * tag, one PARA node) under a fresh temp directory, plus the unscanned
 * source/destination pair the Ingest spec copies between, and writes the
 * GUI's `config.json` so pointing `MAJ_DESKTOP_CONFIG_DIR` at the returned
 * `configDir` opens it.
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

  const ingest = await seedIngestJob(base, majBin, env);

  await mkdir(configDir, { recursive: true });
  await writeFile(
    path.join(configDir, "config.json"),
    JSON.stringify({ catalog: catalogDir }),
  );

  return {
    configDir,
    stateDir,
    catalogDir,
    majBin,
    volumeLabel: VOLUME_LABEL,
    tagName: TAG_NAME,
    photoFileName: `${PHOTO_NAME}.jpg`,
    ...ingest,
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
export function removeTag(fixture: FixtureCatalog, tag: string): void {
  const env = fixtureEnv(fixture);
  const searchJson = runMaj(fixture.majBin, ["search", `tag:${tag}`, "--json"], env);
  const hits = JSON.parse(searchJson) as { results: { asset: string }[] };
  for (const hit of hits.results) {
    runMaj(fixture.majBin, ["tag", "rm", hit.asset, tag], env);
  }
}
