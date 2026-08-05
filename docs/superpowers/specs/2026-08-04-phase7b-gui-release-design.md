# Majestical Phase 7B — GUI slice + release pipeline

Approved in design session 2026-08-04. Implements the remaining two thirds of
the phase 7 spec (`2026-08-02-phase7-agent-surface-gui-design.md`): its GUI
architecture section and its "Release pipeline & CI" section, i.e. Plans B and
C from `HANDOFF-phase7B.md`. Phase 7A shipped the service layer and `maj mcp`;
this phase adds the third head and the delivery machinery. Read
`HANDOFF-phase7B.md` for project state at the start of this phase.

The phase 7 spec's GUI and release sections remain the authoritative
architecture; this spec instantiates them, resolves the details deferred to
"the implementation plan", and adds one new design area — the pre-GUI
watchlist work (chunk 0) the handoff mandates.

## Scope decisions (from design session)

- **GUI slice + release pipeline in one phase** (delivery chunks 6-9 of the
  phase 7 spec), not split. The release pipeline needs the Tauri workspace
  anyway, and the cross-platform CI matrix is required from the first Tauri
  commit.
- **Chunk 0 before any GUI code**: the stderr-diagnostics migration (handoff
  mandate) plus the GUI-adjacent watchlist items — the four schemars-enum
  parameters and the two under-validating dry-run previews. The other 7A
  deferrals (`scan_volume` walk-error count, shared `"ascmhl"` const,
  `IngestRun` rename, the sync-location CLI/MCP divergence) stay on the
  watchlist.
- **`notices: Vec<String>` on outcome structs**, not a structured
  severity-typed notice or a sink callback. All 28 migrated sites are
  diagnostics of the same kind; a plain string list is the YAGNI shape and
  matches the handoff's "outcome-struct fields" direction. It can grow
  structure later if a head needs it.
- GUI stack, layout, command list, first-run flow, custom protocol, workspace
  split, and the whole release-pipeline shape are **as approved in the phase
  7 spec** — restated below only where this spec adds detail.

## Architecture

### Chunk 0 — stderr migration + GUI-adjacent watchlist items

**Notices.** Every `crates/services` outcome struct whose verb can reach one
of the 28 `#[expect(clippy::print_stderr)]` sites (`app.rs` 2, `state_dir.rs`
2, `tags.rs` 1, `search.rs` 6, `inbox.rs` 4, `index/run.rs` 5, `index/heal.rs`
6, `index/mod.rs` 2) gains a `notices: Vec<String>` field,
`#[serde(skip_serializing_if = "Vec::is_empty", default)]` so pinned wire
shapes only change where a notice actually occurs. The `eprintln!` sites
push the identical strings into the outcome instead of printing.

- **Plumbing for pre-outcome sites**: `warn_skipped_corrupt_lines`, the HLC
  clock-clamp warning (`app.rs`), and the legacy-catalog migration notices
  (`state_dir.rs`) fire while opening the catalog, before any verb outcome
  exists. `FsApp` (and the state-dir open path feeding it) accumulates these
  in a notice buffer; each verb drains the buffer into its outcome's
  `notices` before returning. No global state, no callback injection.
- **Head behavior**: the CLI head drains `notices` from the outcome —
  printing each line to stderr verbatim, in original order — *before*
  rendering stdout, so both CLI streams stay byte-identical through the
  migration and the CI reference-binary parity harness proves it exactly as
  it proved the extraction. MCP's `structured_ok` and the GUI serialize the
  field as-is. The 28 `#[expect]` attributes are deleted with their sites.
- **Accepted timing change**: long-running verbs (`index_run` foremost) emit
  their notices at completion instead of live during the run. Final stderr
  bytes are identical; only interactive timing differs. Recorded here so it
  is a decision, not a surprise.

**Schemars enums.** The four enum-shaped string parameters become real Rust
enums defined once in `crates/services` (which takes `schemars` as a new
dependency), deriving `serde` + `JsonSchema`, with serde renames pinning
today's exact wire strings:

| Enum | Variants (wire strings) | Replaces |
| --- | --- | --- |
| `TagOp` | `add`, `rm`, `confirm_suggestion`, `reject_suggestion` | `TagAssetsArgs::op` |
| `ParaOp` | `add`, `rename`, `archive` | `MoveParaArgs::op` |
| `DedupeMode` | `skip`, `copy` | `IngestSourceArgs::dedupe` |
| `DescriberBackend` | `ollama`, `lm-studio`, `open-router` | `SetDescriberArgs::backend` |

The MCP arg structs use them directly (hand-rolled `parse_*`/`bail!` helpers
deleted); the tool-list snapshot updates intentionally to show the enum in
each JSON schema; the GUI later feeds dropdowns from the same types. Where
`crates/cli`'s clap layer already has an equivalent enum, it converts at the
edge — services owns the canonical type.

**Dry-run validation.** `set_metadata` and `tag_assets` get their existence
check in the *dry-run branches* (`crates/cli/src/mcp_cmd/write_tools.rs`),
via the existing `ensure_asset_known` guard exposed from `crates/services`.
An unknown asset id now fails the preview with the same error the
`confirm: true` path produces, instead of promising success. CLI behavior
(`maj meta get` on an unknown id) is deliberately unchanged — the fix is at
the MCP preview layer, so the parity harness is untouched.

### GUI slice — `apps/desktop` (Tauri 2 + Svelte 5 + Vite)

As approved in the phase 7 spec (no SvelteKit; strict tsconfig;
`oxlint`/`oxfmt`/`vitest`; pnpm with exact pins, postinstall blocked,
24-hour minimum release age; current stable versions of everything verified
at execution time). Detail this spec adds:

- **Workspace split**: `apps/desktop/src-tauri` is its own cargo workspace
  (own `Cargo.toml` workspace root + lockfile) with a path dependency on
  `crates/services`. The headless workspace never references it; `just ci`
  gains separate GUI recipes (`gui-check`, `gui-test`, `gui-build`) so
  headless CI jobs stay exactly as fast as today.
- **Commands**: one `#[tauri::command]` per slice verb — `search_assets`,
  `get_asset`, `list_volumes`, `list_saved_searches`, `run_saved_search`,
  `catalog_init` — each parse-nothing wrappers: build request struct, call
  `crates/services`, return the outcome struct serialized as-is (including
  `notices`). Command-level errors map `ServiceError` to a serializable
  error shape naming operation/input/remedy, per the phase-6 polarity.
- **The Lance scoped-thread rule applies to Tauri too.** Tauri commands run
  on a tokio runtime, so `search_assets`/`run_saved_search` (and any future
  command reaching a vector store) must run the service call off-runtime.
  `run_off_tokio_runtime` moves from `crates/cli/src/mcp_cmd/mod.rs` into
  `crates/services` shared plumbing so both heads use the one helper.
- **Notices in the UI**: outcome `notices` render as a visible degradation
  line in the result-count area (Search) or surface header (Volumes) —
  verbatim, never hidden, per the "never swallow a degradation notice"
  rule. This is the payoff of chunk 0: the GUI head starts life with zero
  invisible diagnostics.
- **Layout C** three-pane exactly as approved: sidebar (Search, Volumes —
  no dead buttons), search surface (debounced omnibox with stale-query
  cancellation, saved-search chips, thumbnail grid, timestamped video
  hits), selection-driven inspector (preview/keyframe strip, file facts,
  volume + online state, tags, PARA, verify state; collapses empty),
  read-only Volumes surface (online/offline badge, capacity, asset count,
  last verified).
- **Thumbnails**: `thumb://` Tauri custom protocol reading the blob store
  directly; no image bytes over IPC. Keyframe *images* remain unbuilt
  (watchlist) — the inspector's keyframe strip shows detected timestamps
  from the manifest this phase.
- **First run**: no catalog → welcome screen → "Initialize catalog" →
  `catalog_init` command. No CLI required to reach a working app.
- **Concurrency**: the GUI holds its own SQLite connections (WAL,
  read-heavy slice), safe alongside a running CLI — the guarantee the
  multi-process CLI tests already pin.

### Release pipeline & CI

As approved in the phase 7 spec, restated as commitments with execution
detail:

- Tag push (`v*`) → `tauri-action` builds macOS aarch64 + x86_64 as a
  **draft** release with `latest.json`; a human publishes. No-cache release
  builds.
- **Auto-update armed from day one**: updater keypair generated at setup —
  this is the one step requiring the user (private key + password land as
  GitHub secrets; the plan documents exact commands). `pubkey` + endpoint
  (`releases/latest/download/latest.json`) in `tauri.conf.json`; update
  check wired into the app shell. Signing/notarization degrade gracefully
  when secrets are absent — builds still produce artifacts.
- `cargo-about` license bundle generated in the release job.
- **Version-sync check** across `Cargo.toml` / `tauri.conf.json` /
  `package.json` as a `just` recipe, in CI from the first Tauri commit.
- **Cross-platform build feedback from day one**: the GUI workspace builds
  on macOS, Windows, and Linux (with webkit2gtk deps) in CI from the first
  Tauri commit. Release artifacts stay macOS-only — the matrix is feedback,
  not distribution.
- CI hygiene as established: SHA-pinned actions + version comments,
  `persist-credentials: false`, per-job permissions, zizmor + actionlint
  clean, Dependabot 7-day cooldown groups extended to npm. The four
  conformance jobs and the parity-harness job are untouched.

## Error handling

Nothing new invented. Chunk 0 *widens* the phase-6 doctrine's reach: the 28
formerly-stderr diagnostics become part of the outcome contract, so every
head reports them and none can silently drop them. `ServiceError` mapping in
Tauri commands mirrors MCP's `tool_error`: operator-fixable or total
failures become error states naming setting/file/remedy; per-item failures
stay rows inside successful outcomes; partial progress is always attached.

## Testing

- **Chunk 0**: the CI reference-binary parity harness
  (`services_parity.rs`, merge-base reference) is the proof for the notices
  migration — stdout, stderr, and exit code byte-identical for every
  migrated verb. New mcp_smoke tests pin: a notice-carrying tool result
  (field present with content), the enum schemas in the tool-list snapshot,
  a wrong enum string rejected at the schema/parse layer, and both dry-run
  previews failing on an unknown asset id.
- **GUI**: vitest component tests for the search flow (debounce,
  stale-query cancellation, degradation/notice display, inspector rendering
  from fixture outcomes, welcome flow); Rust-side Tauri command tests over
  a fixture catalog; `tauri_parity` — each command's serialized outcome
  diffed byte-for-byte against the corresponding `maj … --json` stdout over
  the same fixture catalog (the `services_parity.rs` harness shape, applied
  to the third head); `tsc --noEmit` + oxlint + vitest + GUI cargo build in
  the GUI CI recipes.
- **Release**: version-sync recipe has a failing-case test (mismatched
  versions fail CI); workflow lint (zizmor + actionlint) gates the new
  workflows; the release workflow is exercised by a real tag on a
  throwaway prerelease (e.g. `v0.1.0-rc1`) before the phase closes.
- **Mutation testing** on new Rust logic (notices plumbing, enum parsing
  edges, Tauri command wrappers) per project convention at closing;
  reviewers sabotage-probe as established.
- Full WebDriver e2e for the GUI: still watchlist, not this phase.

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **Chunk 0**: notices migration (+ CLI drain + parity proof), schemars
   enums, dry-run validation fixes.
2. Tauri scaffold: `apps/desktop`, GUI workspace split, version-sync
   recipe, GUI CI wiring (3-OS matrix), `thumb://` protocol stub.
3. Search surface + first-run initialize flow + inspector (search-driven).
4. Volumes surface + inspector polish + notices rendering.
5. Release pipeline (tauri-action, updater keypair + arming, cargo-about,
   prerelease dry-run tag) + closing (handoff 7C, watchlist
   reconciliation, mutation sweep).

## Deferred (watchlist items with this spec's attribution)

- Browse / Ingest / Organize surfaces; menu-bar indicator; hover-scrub
  filmstrip; GUI WebDriver e2e; keyframe-image extraction (inspector shows
  manifest timestamps only).
- Windows/Linux release artifacts, signing, notarization credentials,
  distribution; localization.
- The 7A deferrals this phase deliberately does not pick up:
  `scan_volume` dry-run walk-error count, shared `"ascmhl"` const,
  `IngestRun` → `IngestOutcome` rename, the `list_sync_locations`
  CLI/MCP missing-catalog divergence, MCP long-running progress
  notifications, `maj doctor`.
- Live (streaming) notice delivery for long-running verbs — chunk 0
  delivers notices at completion; if GUI UX needs mid-run progress, that
  rides the MCP progress-notification item above.

## As-built (phase 7B)

What shipped, where it differs from the design above. Written as what IS,
not as a change log.

**Two commands and a config file the design did not name.** The GUI needs to
answer "which catalog, and is it usable" before it renders anything, and it
needs to remember the answer across launches. `app_status` reports the
selected catalog's path and whether it opens; `use_existing_catalog` adopts a
catalog the user already has, beside `initialize_catalog` which creates one.
Between them they are what the first-run surface is built from. The choice
persists in a small JSON file under the platform config directory
(`apps/desktop/src-tauri/src/config.rs`), read once at startup by
`restore_persisted_catalog`. A config naming a catalog that has since
disappeared is not an error: the state carries it and `app_status` reports
`catalog_ready: false`, which is exactly what the first-run surface renders.

**Registering a Tauri plugin and configuring it are one edit, not two.** The
updater plugin's config has no default for `pubkey`, so
`.plugin(tauri_plugin_updater::Builder::new().build())` without a
`plugins.updater` block in `tauri.conf.json` fails to deserialize its
configuration and the app exits on startup instead of opening a window. The
two must land together. Arming the updater turned out to be three edits, not
two: the plugin registration, the `plugins.updater` block, and
`bundle.createUpdaterArtifacts: true` — without the third the bundler
produces no `.tar.gz`/`.sig` pair and `latest.json` ships listing no
platforms. All three are documented in `docs/RELEASING.md`.

**The services graph is macOS-only, and that gates the CI matrix.**
`crates/index` depends on `objc2`, Vision and PDFKit unconditionally, so
everything downstream of it — including the Tauri app — builds on macOS
alone. The CI matrix is therefore 3-OS for the frontend gates (`pnpm
check`/`lint`/`test`/`build`, which are genuinely cross-platform) and
macOS-only for every Rust step. Discovered by the first matrix run on #77.

**The notices mechanism as built.** Chunk 0's design held; three refinements
are worth stating. Sync uses a local sink rather than the ambient one, which
keeps a pull's notices attached to that pull — at the cost that a call
failing before its drain loses them (recorded as a 7B deferral, with
`pull_impl` the case that would gain most from a notices payload on
`ServiceError`). `maj mcp` folds notices into each structured result through
a `with_notices` helper rather than each tool doing it by hand. The GUI's
`searches_list` is a thin wrapper around the service call because the GUI
wants the saved searches and the notices together; the CLI drains at
end-of-command instead.

**The schemars-enum snapshot correction.** The plan assumed the MCP schema
snapshot in `mcp_smoke.rs` pinned these four parameters and would need
updating. It did not pin them at all — the snapshot was silent about their
type. So the work was not "update the snapshot" but "add the pin": a tripwire
asserting each enum's allowed values, without which the derive could be
dropped again with nothing failing.

**Vacuous tests the plan carried, corrected at the source.** Two patterns
were found and fixed where they were written, not worked around.
`waitFor(() => expect(x).toBeNull())` passes on its first check, before the
thing it is meant to exclude has had any chance to arrive — the assertions
that need to prove absence now wait a fixed interval first and then assert.
And a test that clears state before asserting a failure can mask the failure
it exists to catch. `styles.test.ts` carries the third: it asserts computed
layout, which is only meaningful if the real stylesheet is in the document,
so a `beforeAll` guard fails the suite if the sheet is empty — the vitest
default hands CSS back as an empty string, under which every assertion in the
file would otherwise pass against no stylesheet at all.

**Smaller divergences.** `PassEnv` was renamed for what it does rather than
how it is passed. Blob reading was extracted to
`crates/services/src/index/blobs.rs` so `maj mcp`'s `majestical://` resources
and the app's `thumb://` protocol share one lookup and one remedy text
instead of a copy each. Svelte's `{#each}` over notices is deliberately
unkeyed: the same notice can legitimately arrive twice in one outcome (a
saved-search run drains the same corrupt-log warning from both the projection
load and the catalog open), and a keyed each throws on the repeat instead of
rendering it. The shared vitest mocking helpers live in
`apps/desktop/src/lib/test-support.ts`.
