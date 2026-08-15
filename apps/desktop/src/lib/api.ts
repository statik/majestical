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
