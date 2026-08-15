import { describe, expect, it } from "vitest";
import type {
  AppStatus,
  ArchiveOutcome,
  AssetDetail,
  AssignOutcome,
  BrowseListOutcome,
  BrowseTreeOutcome,
  CommandError,
  MountedRoot,
  MoveStatus,
  ParaOutcome,
  SavedSearches,
  SearchOutcome,
  TagRenameOutcome,
  TagsListOutcome,
  VolumesOutcome,
} from "./api";
import appStatus from "./fixtures/app_status.json";
import archiveOutcome from "./fixtures/archive_outcome.json";
import assetDetail from "./fixtures/asset_detail.json";
import assignOutcome from "./fixtures/assign_outcome.json";
import browseList from "./fixtures/browse_list.json";
import browseTree from "./fixtures/browse_tree.json";
import commandError from "./fixtures/command_error.json";
import mountedRoots from "./fixtures/mounted_roots.json";
import paraOutcome from "./fixtures/para_outcome.json";
import savedSearches from "./fixtures/saved_searches.json";
import searchOutcome from "./fixtures/search_outcome.json";
import tagRenameOutcome from "./fixtures/tag_rename_outcome.json";
import tagsListOutcome from "./fixtures/tags_list_outcome.json";
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
const typedBrowseTree: BrowseTreeOutcome = browseTree;
const typedBrowseList: BrowseListOutcome = browseList;
const typedTagsListOutcome: TagsListOutcome = tagsListOutcome;
const typedTagRenameOutcome: TagRenameOutcome = tagRenameOutcome;
const typedAssignOutcome: AssignOutcome = assignOutcome;
const typedParaOutcome: ParaOutcome = paraOutcome;
// `ArchiveMove.status` is a string-literal union (`MoveStatus`); JSON module
// inference widens it to `string`, same reason `AssetDetail` above needs a
// cast rather than a plain assignment.
const typedArchiveOutcome: ArchiveOutcome = archiveOutcome as ArchiveOutcome;
// The cast above means the fixture can't pin the union's VALUES (the JSON
// only carries "moved", and widening accepts any string) — this typed
// literal is what breaks compilation if a `MoveStatus` variant drifts from
// para.rs's `#[serde(rename_all = "snake_case")]` spelling.
const allMoveStatuses: MoveStatus[] = ["moved", "already_archived", "planned"];
// A bare array on the wire — `list_mounted_roots` reads the mount table, so
// it has no outcome struct and no notices to carry.
const typedMountedRoots: MountedRoot[] = mountedRoots;

describe("wire fixtures", () => {
  it("carry the load-bearing runtime shapes", () => {
    expect(typedCommandError.notices?.length).toBeGreaterThan(0);
    expect(typedSearchOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedAssetDetail.notices?.length).toBeGreaterThan(0);
    expect(typedAppStatus.catalog_path.length).toBeGreaterThan(0);
    expect(typedVolumesOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedSavedSearches.saved.length).toBeGreaterThan(0);
    expect(typedSavedSearches.notices?.length).toBeGreaterThan(0);
    expect(typedBrowseTree.notices?.length).toBeGreaterThan(0);
    expect(typedBrowseList.notices?.length).toBeGreaterThan(0);
    expect(typedBrowseTree.volumes[0]?.folders.length).toBeGreaterThan(0);
    expect(typedTagsListOutcome.tags.length).toBeGreaterThan(0);
    expect(typedTagsListOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedTagRenameOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedAssignOutcome.failed.length).toBeGreaterThan(0);
    expect(typedAssignOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedParaOutcome.nodes.length).toBeGreaterThan(0);
    expect(typedParaOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedArchiveOutcome.moves.length).toBeGreaterThan(0);
    expect(typedArchiveOutcome.notices?.length).toBeGreaterThan(0);
    expect(typedArchiveOutcome.moves[0]?.status).toBe("moved");
    expect(typedArchiveOutcome.moves[1]?.status).toBe("already_archived");
    expect(allMoveStatuses).toContain(typedArchiveOutcome.moves[0]?.status);
    expect(typedMountedRoots[0]?.path.length).toBeGreaterThan(0);
  });

  // An optional interface field can vanish from a regenerated fixture (a
  // serde rename) without breaking the assignment above — a missing
  // optional is still legal TS, so only a runtime assert here catches it.
  it("carry the optional fields the compile-time assignment cannot enforce", () => {
    expect(typedSearchOutcome.semantic_coverage).toBeDefined();
    expect(typedSearchOutcome.text_coverage?.length).toBeGreaterThan(0);
    const [hit] = typedSearchOutcome.results;
    expect(hit?.timestamp_ms).toBeDefined();
    expect(hit?.source).toBeDefined();
    expect(hit?.locator).toBeDefined();
    expect(hit?.snippet).toBeDefined();
    const [browseHit] = typedBrowseList.results;
    expect(browseHit?.size).toBeDefined();
    expect(browseHit?.mtime_ms).toBeDefined();
    expect(browseHit?.kind).toBeDefined();
  });
});
