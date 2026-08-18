// The wire contract with `src-tauri/src/commands.rs`: one wrapper per command
// and one interface per outcome struct, mirroring the Rust field-for-field
// (snake_case included — these are serde's names, not ours to prettify).
// Fields the Rust skips when empty (`skip_serializing_if`) are optional here.
// Every interface here is pinned by `fixtures.test.ts` against a fixture in
// `fixtures/*.json`; a new outcome interface needs a builder in
// `src-tauri/tests/wire_fixtures.rs` too.
import { invoke } from "@tauri-apps/api/core";

/** `majestical_services::search::VolumeRef` */
export interface VolumeRef {
  id: string;
  label: string;
  online: boolean;
}

/**
 * `majestical_services::search::SearchHit`. When `known` is false the catalog
 * no longer knows this asset and `name`/`volumes`/`tags`/`para` are
 * placeholders, not a genuinely empty summary — render the id alone, as the
 * CLI does.
 */
export interface SearchHit {
  asset: string;
  score: number;
  known: boolean;
  name: string;
  volumes: VolumeRef[];
  tags: string[];
  para: string | null;
  /**
   * Populated by a search hit's semantic match; browse rows never carry
   * these — they're `Some` only in the synthetic wire fixture (see
   * `wire_fixtures.rs`'s `browse_list_fixture`) so the TS side type-checks
   * the full surface.
   */
  timestamp_ms?: number;
  source?: string;
  locator?: number;
  snippet?: string;
  /** Browse populates size/mtime_ms/kind; search leaves them absent. */
  size?: number;
  mtime_ms?: number;
  kind?: string;
}

/** `majestical_services::search::SemanticCoverage` */
export interface SemanticCoverage {
  embedded: number;
  eligible: number;
}

/** `majestical_services::search::TextCoverageNotice` */
export interface TextCoverageNotice {
  label: string;
  noun: string;
  covered: number;
  eligible: number;
  remedy: string;
  source: string;
}

/** `majestical_services::search::SearchOutcome` */
export interface SearchOutcome {
  count: number;
  results: SearchHit[];
  semantic_coverage?: SemanticCoverage;
  text_coverage?: TextCoverageNotice[];
  notices?: string[];
}

/** `majestical_services::search::SavedSearch` */
export interface SavedSearch {
  name: string;
  query: string;
}

/** `commands::SavedSearches` */
export interface SavedSearches {
  saved: SavedSearch[];
  notices?: string[];
}

/** `majestical_services::volumes::VolumeRow` */
export interface VolumeRow {
  id: string;
  label: string;
  last_seen_ms: number;
  online: boolean;
  asset_count: number;
  clock_suspect: boolean;
}

/** `majestical_services::volumes::VolumesOutcome` */
export interface VolumesOutcome {
  volumes: VolumeRow[];
  notices?: string[];
}

/** `majestical_services::browse::BrowseFolder` */
export interface BrowseFolder {
  path: string;
  children: string[];
  recursive_count: number;
}

/** `majestical_services::browse::BrowseVolume` */
export interface BrowseVolume {
  id: string;
  label: string;
  online: boolean;
  folders: BrowseFolder[];
}

/** `majestical_services::browse::BrowseTreeOutcome` */
export interface BrowseTreeOutcome {
  volumes: BrowseVolume[];
  notices?: string[];
}

/** `majestical_services::browse::BrowseListOutcome` */
export interface BrowseListOutcome {
  count: number;
  folder_count: number;
  results: SearchHit[];
  notices?: string[];
}

/**
 * `browse_list`'s `sort` values — mirrors `SORT_VALUES` in
 * `majestical_services::browse` (browse.rs), the same source of truth the
 * Rust side validates against.
 */
export type BrowseSort = "captured" | "name" | "size";

/**
 * `browse_list`'s `kind` filter values — mirrors
 * `majestical_core::media_kind::MediaKind::ALL`.
 */
export type BrowseKind = "image" | "video" | "audio" | "pdf" | "other";

/** `majestical_services::catalog::AssetInstance` */
export interface AssetInstance {
  volume: string;
  volume_label: string;
  online: boolean;
  path: string;
  size: number;
  mtime_ms: number;
}

/** `majestical_core::event::VerifyOutcome`, which serializes snake_case. */
export type VerifyOutcome = "original" | "verified" | "failed";

/** `majestical_services::catalog::AssetVerification` */
export interface AssetVerification {
  volume: string;
  path: string;
  algo: string;
  value: string;
  outcome: VerifyOutcome;
  hashdate_ms: number;
}

/**
 * `majestical_services::catalog::AssetDetail`. `fields` is a `Vec<(String,
 * String)>` in Rust but serializes as a JSON object (`meta::
 * serialize_pairs_as_map`), so it arrives here as a record.
 */
export interface AssetDetail {
  asset: string;
  instances: AssetInstance[];
  tags: string[];
  para: string | null;
  fields: Record<string, string>;
  verifications: AssetVerification[];
  has_thumb: boolean;
  notices?: string[];
}

/** `commands::AppStatus`. An empty `catalog_path` means none chosen yet. */
export interface AppStatus {
  catalog_path: string;
  catalog_ready: boolean;
}

/** `majestical_services::tags::TagRow` */
export interface TagRow {
  tag: string;
  count: number;
  last_used_ms: number;
}

/** `majestical_services::tags::TagsListOutcome` */
export interface TagsListOutcome {
  tags: TagRow[];
  notices?: string[];
}

/** `majestical_services::tags::TagRenameOutcome` — `tag_rename` and `tag_merge` share this shape. */
export interface TagRenameOutcome {
  from: string;
  to: string;
  rewritten: number;
  notices?: string[];
}

/** `majestical_services::tags::AssignFailure` */
export interface AssignFailure {
  asset: string;
  reason: string;
}

/** `majestical_services::tags::AssignOutcome` — shared by `assign_tags` and `file_assets`. */
export interface AssignOutcome {
  applied: number;
  failed: AssignFailure[];
  notices?: string[];
}

/** `majestical_services::para::ParaNodeRow` */
export interface ParaNodeRow {
  id: string;
  kind: string;
  name: string;
  archived: boolean;
}

/** `majestical_services::para::ParaOutcome` */
export interface ParaOutcome {
  nodes: ParaNodeRow[];
  notices?: string[];
}

/** `majestical_services::para::MoveStatus`, which serializes snake_case. */
export type MoveStatus = "moved" | "already_archived" | "planned";

/** `majestical_services::para::ArchiveMove` */
export interface ArchiveMove {
  from: string;
  to: string;
  status: MoveStatus;
}

/** `majestical_services::para::ArchiveOutcome` */
export interface ArchiveOutcome {
  moves: ArchiveMove[];
  executed: boolean;
  notices?: string[];
}

/**
 * `commands::MountedRoot`. The archive modal's candidate roots: nothing in
 * the catalog records where a node was materialized, so the roots a dry run
 * is planned against are the volumes plugged in right now.
 */
export interface MountedRoot {
  volume: string;
  label: string;
  path: string;
}

/**
 * `majestical_ingest::plan::Decision` — serde tag `decision`, snake_case.
 * `duplicate.action` is `plan::DedupeMode`; this head always plans and
 * copies with `skip` (`commands::INGEST_DEDUPE`), but the other two spellings
 * are part of the type a plan can carry.
 */
export type IngestDecision =
  | { decision: "copy" }
  | {
      decision: "duplicate";
      asset: string;
      action: "skip" | "copy_anyway" | "link";
    }
  | { decision: "rejected"; reason: string };

/**
 * `majestical_ingest::plan::PlannedFile`. `prehash` is `null` — not absent —
 * for a file the planner never hashed (its size matched nothing the catalog
 * knows, so no dedupe check was needed).
 */
export interface PlannedFile {
  source: string;
  rel: string;
  size: number;
  prehash: string | null;
  decision: IngestDecision;
}

/** `majestical_ingest::plan::IngestPlan` */
export interface IngestPlan {
  files: PlannedFile[];
}

/**
 * `majestical_services::ingest::IngestPlanOutcome` — what `plan_ingest`
 * returns. `subdir` is the layout template already rendered
 * (`<KindDir>/<name>/<template>`), relative to each destination root.
 */
export interface IngestPlanOutcome {
  plan: IngestPlan;
  subdir: string;
  node_id: string;
  source_volume_id: string;
  source_volume_label: string;
  notices?: string[];
}

/** `majestical_ingest::engine::PlacedFile` */
export interface PlacedFile {
  rel: string;
  size: number;
  xxh3: string;
  xxh64: string;
  /** Final path under every destination root, `/`-separated. */
  dest_rel: string;
}

/** `majestical_ingest::engine::FailedFile` — `failed` and `rejected` rows. */
export interface FailedFile {
  rel: string;
  reason: string;
}

/**
 * `majestical_ingest::engine::Outcome`. `diagnostics` are engine-internal
 * notes about no particular file (recovered lock poisoning), kept apart from
 * `failed` so a per-file list never shows one.
 */
export interface IngestOutcome {
  placed: PlacedFile[];
  failed: FailedFile[];
  skipped_duplicates: string[];
  rejected: FailedFile[];
  skipped_resumed: number;
  diagnostics: string[];
}

/** `majestical_ingest::mhl::WrittenGeneration` */
export interface WrittenGeneration {
  path: string;
  generation: number;
  /** c4 hash of the manifest's own bytes, as recorded in the ASC MHL chain. */
  roothash: string;
}

/**
 * `majestical_services::ingest::IngestRun` — a finished run. `generations`
 * is a `Vec<(PathBuf, WrittenGeneration)>` in Rust, so it arrives as pairs of
 * [destination root, generation]: one per destination that got new files.
 *
 * This — not the accumulated progress events — is the authority on what a
 * run placed: the engine's end-of-run sweep can demote a file it already
 * announced as `file_placed`, and that demotion appears only here.
 */
export interface IngestRun {
  run_id: string;
  outcome: IngestOutcome;
  generations: [string, WrittenGeneration][];
  notices?: string[];
}

/** `majestical_services::ingest::UnfinishedRun` — a resume candidate. */
export interface UnfinishedRun {
  run_id: string;
  placed: number;
  planned: number;
  source: string;
  destinations: string[];
}

/** `majestical_services::ingest::UnfinishedRunsOutcome`, newest run first. */
export interface UnfinishedRunsOutcome {
  runs: UnfinishedRun[];
  notices?: string[];
}

/**
 * `majestical_ingest::engine::ProgressEvent` — serde tag `type`, snake_case.
 *
 * Ordered within one file: `file_started`, then its `bytes_copied` (this head
 * coalesces them — see `ingest::BytesThrottle`), then one `file_verified`
 * per destination, then exactly one `file_placed` or `file_failed`. Files
 * interleave freely; several workers copy at once.
 *
 * `run_stopped` means the copy loop ended, NOT that the outcome is ready:
 * the engine's missing-file sweep, the ASC MHL generation per destination,
 * and the catalog events all land after it — seconds later on a big run.
 * A surface that saw it polls `ingestState()` until `busy` is false and
 * renders `finished` then, rather than treating `run_stopped` as the end.
 */
export type ProgressEvent =
  | { type: "run_started"; files_total: number; bytes_total: number }
  | { type: "file_started"; rel: string; size: number }
  | { type: "bytes_copied"; rel: string; bytes_done: number }
  | { type: "file_verified"; rel: string; dest_root: string }
  | { type: "file_placed"; rel: string }
  | { type: "file_failed"; rel: string; reason: string }
  | { type: "run_stopped"; cancelled: boolean };

/**
 * `commands::IngestProgress` — the payload of every
 * [`INGEST_PROGRESS_EVENT`]. The run id rides the envelope because the event
 * itself carries none, and a surface that mounted mid-run has to know which
 * run it is watching.
 */
export interface IngestProgress {
  run_id: string;
  event: ProgressEvent;
}

/** The Tauri event name `start_ingest` forwards progress under. */
export const INGEST_PROGRESS_EVENT = "ingest-progress";

/**
 * `commands::FinishedIngest` — how the last run ended. A failure is a value
 * the state keeps, not a lost promise: the run outlives the webview, so the
 * error that ended it survives a reload too.
 */
export type FinishedIngest =
  | { status: "done"; run: IngestRun }
  | { status: "failed"; error: CommandError };

/**
 * `commands::IngestStateWire` — what the Ingest surface reads on mount.
 *
 * `busy`, not `running`, is the authority on whether Start can be offered:
 * `running` names the in-flight run and is absent both when nothing runs and
 * for the instant between `start_ingest` claiming the job slot and the run
 * naming itself.
 */
export interface IngestState {
  running?: string;
  busy: boolean;
  finished?: FinishedIngest;
}

/**
 * Argument names are camelCase because `#[tauri::command]` defaults to
 * `rename_all = "camelCase"` — Rust's `asset_id` is looked up as `assetId`.
 *
 * Neither search wrapper passes `limit`, and `browseList` doesn't either: the
 * commands already default it (`commands::DEFAULT_LIMIT` /
 * `browse::DEFAULT_LIMIT`), and a second copy of that number here would be
 * a second place to change it.
 */
export const api = {
  appStatus: () => invoke<AppStatus>("app_status"),
  searchAssets: (query: string) =>
    invoke<SearchOutcome>("search_assets", { query }),
  runSavedSearch: (name: string) =>
    invoke<SearchOutcome>("run_saved_search", { name }),
  getAsset: (assetId: string) =>
    invoke<AssetDetail | null>("get_asset", { assetId }),
  listVolumes: () => invoke<VolumesOutcome>("list_volumes"),
  listSavedSearches: () => invoke<SavedSearches>("list_saved_searches"),
  browseTree: () => invoke<BrowseTreeOutcome>("browse_tree"),
  browseList: (req: {
    volume: string;
    path?: string | undefined;
    flatten?: boolean | undefined;
    sort?: BrowseSort | undefined;
    kind?: BrowseKind | undefined;
    offset?: number | undefined;
  }) => invoke<BrowseListOutcome>("browse_list", req),
  initializeCatalog: (path: string) =>
    invoke<AppStatus>("initialize_catalog", { path }),
  useExistingCatalog: (path: string) =>
    invoke<AppStatus>("use_existing_catalog", { path }),
  listTags: () => invoke<TagsListOutcome>("list_tags"),
  renameTag: (from: string, to: string) =>
    invoke<TagRenameOutcome>("rename_tag", { from, to }),
  // The Rust command's parameter is named `into_tag` (`into` is a reserved
  // keyword), so Tauri's default camelCase renders the wire key as
  // `intoTag` — see `commands::merge_tags`'s own comment for the other half.
  mergeTags: (from: string, into: string) =>
    invoke<TagRenameOutcome>("merge_tags", { from, intoTag: into }),
  assignTags: (assetIds: string[], tags: string[]) =>
    invoke<AssignOutcome>("assign_tags", { assetIds, tags }),
  fileAssets: (assetIds: string[], node: string) =>
    invoke<AssignOutcome>("file_assets", { assetIds, node }),
  listPara: () => invoke<ParaOutcome>("list_para"),
  addParaNode: (kind: string, name: string) =>
    invoke<string>("add_para_node", { kind, name }),
  renameParaNode: (node: string, name: string) =>
    invoke<void>("rename_para_node", { node, name }),
  archiveNode: (node: string, roots: string[], dryRun: boolean) =>
    invoke<ArchiveOutcome>("archive_node", { node, roots, dryRun }),
  listMountedRoots: () => invoke<MountedRoot[]>("list_mounted_roots"),
  // Ingest. `planIngest` takes no destinations: the plan is a diff of the
  // source against the catalog, so it doesn't depend on where the bytes
  // will land — only `startIngest` does. Both default `template` to
  // `commands::DEFAULT_INGEST_TEMPLATE` when it is left out, the same
  // string `maj ingest` and MCP's `ingest_source` default to.
  planIngest: (req: {
    source: string;
    para: string;
    template?: string | undefined;
  }) => invoke<IngestPlanOutcome>("plan_ingest", req),
  // Resolves with the run id once the run has one — after its planning
  // pass, before its first byte. The copy itself keeps going on the
  // backend's own thread; progress arrives as INGEST_PROGRESS_EVENT.
  startIngest: (req: {
    source: string;
    dests: string[];
    para: string;
    template?: string | undefined;
    resume?: string | undefined;
  }) => invoke<string>("start_ingest", req),
  cancelIngest: () => invoke<void>("cancel_ingest"),
  ingestState: () => invoke<IngestState>("ingest_state"),
  listUnfinishedIngests: () =>
    invoke<UnfinishedRunsOutcome>("list_unfinished_ingests"),
};

/**
 * The wire shape of a rejected command — `commands::CommandError`.
 * `notices` is absent (not `[]`) when the failing call collected none.
 */
export interface CommandError {
  message: string;
  notices?: string[];
}

/** The notices a rejected command carried, `[]` when none. */
export function errorNotices(error: unknown): string[] {
  if (typeof error === "object" && error !== null && "notices" in error) {
    const { notices } = error as { notices: unknown };
    if (
      Array.isArray(notices) &&
      notices.every((n) => typeof n === "string")
    ) {
      return notices;
    }
  }
  return [];
}

/**
 * A rejected command carries `commands::CommandError`, whose `message` is the
 * whole `{err:#}` chain — the remedy text is already in there, so surfaces
 * show it whole rather than summarizing it away.
 */
export function errorMessage(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error) {
    const { message } = error as { message: unknown };
    if (typeof message === "string") {
      return message;
    }
  }
  return String(error);
}
