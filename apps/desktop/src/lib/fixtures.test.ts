import { describe, expect, it } from "vitest";
import type {
  AppStatus,
  AssetDetail,
  CommandError,
  SavedSearches,
  SearchOutcome,
  VolumesOutcome,
} from "./api";
import appStatus from "./fixtures/app_status.json";
import assetDetail from "./fixtures/asset_detail.json";
import commandError from "./fixtures/command_error.json";
import savedSearches from "./fixtures/saved_searches.json";
import searchOutcome from "./fixtures/search_outcome.json";
import volumesOutcome from "./fixtures/volumes_outcome.json";

// The assignments ARE the test: a serde rename in Rust regenerates the
// fixture, and the stale interface fails `svelte-check`/`tsc` right here.
const typedAppStatus: AppStatus = appStatus;
const typedSearchOutcome: SearchOutcome = searchOutcome;
// `AssetVerification.outcome` is a string-literal union (`VerifyOutcome`);
// JSON module inference widens string literals to `string`, so the field
// name/shape is still pinned but this one value's literal-ness is not. A
// cast, not a plain assignment, is required for exactly that reason.
const typedAssetDetail: AssetDetail = assetDetail as AssetDetail;
const typedVolumesOutcome: VolumesOutcome = volumesOutcome;
const typedSavedSearches: SavedSearches = savedSearches;
const typedCommandError: CommandError = commandError;

describe("wire fixtures", () => {
  it("carry the load-bearing runtime shapes", () => {
    expect(typedCommandError.notices?.length).toBeGreaterThan(0);
    expect(typedSearchOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedAssetDetail.notices?.length).toBeGreaterThan(0);
    expect(typedAppStatus.catalog_path.length).toBeGreaterThan(0);
    expect(typedVolumesOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedSavedSearches.saved.length).toBeGreaterThan(0);
    expect(typedSavedSearches.notices?.length).toBeGreaterThan(0);
  });
});
