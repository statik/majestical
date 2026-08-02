# Majestical Phase 7 — Agent surface (`maj mcp`) + GUI slice

Approved in design session 2026-08-02. Implements step 7 of the parent spec's
build order (`2026-07-28-majestical-design.md` §6-§7). Read
`HANDOFF-phase7.md` for project state at the start of this phase.

Goal: three heads over one service core. Extract the CLI's per-verb
operations into a `crates/services` layer, ship a full-parity MCP server as
`maj mcp`, and land the first Tauri + Svelte GUI slice (Search + Volumes)
with the complete release pipeline armed.

## Scope decisions (from design session)

- **MCP + thin GUI slice in one phase.** Not MCP-only: the GUI slice proves
  the parity-by-construction claim while the service extraction is fresh.
- **Full CLI parity for MCP tools.** Every verb, not a curated core set —
  agents can achieve any outcome users can. `catalog_init` included.
- **Official `rmcp` SDK** (modelcontextprotocol/rust-sdk), not hand-rolled
  JSON-RPC. Version verified at execution time.
- **New `crates/services` workspace crate** for the extracted operations.
  Not cli-as-lib (GUI would inherit clap), not duplication.
- **Full release pipeline now**: tag → tauri-action draft release,
  auto-update armed from day one, cargo-about bundle, version-sync check.
- **GUI slice = Search + Volumes**, layout C (three-pane with inspector),
  wired via in-process Tauri commands (not sidecar CLI, not GUI-speaks-MCP).
- **No CLI required to initialize**: the GUI first-run flow initializes the
  catalog itself through the same `catalog_init` service verb.

## Architecture

### Service layer — `crates/services` (new)

One function per verb: request struct in, serde-serializable outcome struct
out, `thiserror` error enum carrying operation/input/suggested fix. This
generalizes the phase-6 `run_ingest`/`IngestRun`/`IngestReport::Silent`
pattern. Heads (CLI, MCP, GUI) render outcomes; they never re-implement
operations.

Verbs: `search`, `get_asset`, `volumes_list`, `verify_volume`, `scan`,
`meta`, `ingest`, `tag_add`/`tag_rm`/`tags_list`,
`para_add`/`para_move`/`para_archive`, saved searches (list/save/run),
`sync_push`/`sync_pull`/`sync_status`, sync locations (add/list/rm),
`inbox_process`, `index_run`/`index_status`, describer (show/set/test),
`catalog_init`. The implementation plan enumerates the authoritative
inventory from `maj --help` at plan time; the parity rule (every CLI verb
gets a service function and an MCP tool) wins over any omission in this
list.

Migration rules:

- **Incremental per verb, never big-bang.** Each extraction chunk moves 1-3
  verbs and keeps `just ci` green.
- **Pinned JSON contracts stay byte-identical.** The phase 5-6 smoke suites
  (search rows, sync location rows, pull summaries, inbox report rows) are
  the proof; each chunk runs them before and after. Any intentional contract
  change is out of scope for the extraction.
- CLI `cmd_*` functions become: parse args → build request → call service →
  render text/JSON. Rendering stays in `crates/cli`.
- **Exit-code polarity lives in the outcome structs, decided once.**
  Services return per-item failure rows and overall status per the phase-6
  doctrine (contributor-side faults converge to recorded-notice; operator-
  fixable faults are fresh failures every pass; partial progress always kept
  and reported). Heads map that to exit codes (CLI), tool results (MCP), or
  error states (GUI) without re-deriving it.
- `crates/services` depends on core/catalog-sqlite/ingest/sync/index/
  describe; `crates/cli` and the Tauri backend depend on `crates/services`.

### MCP server — `maj mcp` (subcommand in `crates/cli`)

- stdio transport; launched as `maj mcp` from any MCP client config. Single
  binary keeps the agent story simple.
- Built on `rmcp`; a tokio runtime starts only for this subcommand — the
  rest of the CLI stays sync.
- Tool parameter schemas derive from the same request structs the service
  layer defines, so CLI flags and MCP parameters cannot drift.

Tools (names per parent spec §6 where it names them):

| Tool | Kind | Notes |
| --- | --- | --- |
| `search_assets` | read | query/filters/limit; rows identical to `maj search --json`, incl. timestamped video hits |
| `get_asset` | read | metadata, tags, PARA, volume + online state, verify state |
| `list_volumes` | read | the shelf: online/offline, last verified |
| `list_saved_searches` / `run_saved_search` | read | saved-search parity |
| `sync_status` / `index_status` | read | same shapes as CLI JSON |
| `list_sync_locations` / `get_describer` | read | config surfaces |
| `ingest_source` | mutating | verified multi-destination ingest |
| `tag_assets` | mutating | add/rm in one tool, `op` parameter |
| `move_para` | mutating | PARA add/move/archive |
| `verify_volume` | mutating, long | re-verify against ASC MHL |
| `sync_push` / `sync_pull` | mutating | per-location outcome rows |
| `inbox_process` | mutating | contributions + triage; `keep` flag |
| `index_run` | mutating, long | diff-as-queue indexing |
| `add_sync_location` / `rm_sync_location` | mutating | location config |
| `scan_volume` / metadata verbs | mutating | read/write split per plan inventory; reads stay confirm-free |
| `set_describer` / `test_describer` | mutating | backend config + probe |
| `catalog_init` | mutating | always requires `confirm` |

The table follows the same parity rule as the verb list: any CLI verb the
plan's inventory surfaces that is missing here still gets a tool.

- **Confirm semantics** (parent spec §6): every mutating tool takes
  `confirm` defaulting to `false` → returns a dry-run diff (phase-6 dry-run
  hooks) describing exactly what would happen. `confirm: true` executes.
  Read tools have no confirm parameter.
- **Failure mapping** mirrors the phase-6 polarity: per-item failures return
  inside a successful tool result as failure rows; only operator-fixable or
  total failures become MCP tool errors, message naming setting/file/remedy.
- **Resources**: `majestical://thumb/{asset_id}` and
  `majestical://keyframe/{asset_id}/{index}` served from the blob store so
  agents can see results. Asset IDs + timestamps in every result are stable
  for chaining.

### GUI slice — `apps/desktop` (Tauri 2 + Svelte 5 + Vite)

- No SvelteKit: desktop app, two surfaces, no SSR/routing need. TypeScript
  with the full strict tsconfig; `oxlint`/`oxfmt`/`vitest`; pnpm with exact
  pins and postinstall scripts blocked. Current stable versions of
  Tauri/Svelte/Vite verified at execution time.
- `src-tauri` lives in the **GUI workspace** (split headless/GUI workspaces
  per parent spec §7) so headless CI never compiles the Tauri tree.
- Tauri backend depends on `crates/services`; one `#[tauri::command]` per
  verb the slice uses: `search_assets`, `get_asset`, `list_volumes`,
  `list_saved_searches`, `run_saved_search`, `catalog_init`. Commands return
  service outcome structs serialized as-is (parity by construction).
- Thumbnails/keyframes via a Tauri custom protocol (`thumb://`) reading the
  blob store directly; no image bytes over IPC.
- **Layout C** (three-pane with inspector, selected from wireframes):
  - Sidebar: Search and Volumes only. Future surfaces do not appear as
    dead buttons (no phantom features).
  - Search surface: debounced omnibox with stale-query cancellation; result
    count line showing degradation notices verbatim from the service
    (offline volumes, index gaps — never hidden); saved-search chips;
    thumbnail results grid; video hits show timestamped matches.
  - Inspector: selection-driven right pane — preview/keyframe strip,
    filename/size/dates, volume + online state, tags, PARA location, verify
    state + last-verified date. Collapses when nothing is selected.
  - Volumes surface: every volume with online/offline badge, capacity,
    asset count, last verified. Read-only this phase.
- **First run**: if no catalog exists, a welcome flow offers "Initialize
  catalog" backed by the `catalog_init` service verb via its Tauri command.
  No CLI required to reach a working app.
- Concurrency: the GUI holds its own SQLite connections (WAL; read-heavy
  slice) and is safe alongside a running CLI — the guarantee the existing
  multi-process CLI tests already pin.

## Release pipeline & CI

Cuesheet patterns with the gaps closed (parent spec §7), all in this phase:

- Tag push → `tauri-action` builds macOS aarch64 + x86_64 as a **draft**
  release with `latest.json`; a human publishes. No-cache release builds.
- **Auto-update armed from day one**: updater keypair generated at setup
  (private key + password stored as GitHub secrets — requires the user;
  exact steps documented in the plan), `pubkey` + endpoint
  (`releases/latest/download/latest.json`) in `tauri.conf.json`, update
  check wired into the app shell. Signing/notarization degrade gracefully
  when secrets are absent — builds still produce artifacts.
- `cargo-about` license bundle generated in the release job.
- **Version-sync check** across Cargo.toml / tauri.conf.json / package.json
  as a `just` recipe, in CI from the first Tauri commit.
- CI hygiene as established: SHA-pinned actions + version comments,
  `persist-credentials: false`, per-job permissions, zizmor + actionlint,
  Dependabot 7-day cooldown groups extended to npm/GUI. The four
  conformance jobs are untouched.

## Error handling

Nothing new invented. Services carry the phase-6 doctrine — typed errors
naming operation/input/remedy; counts from real files/rows; partial progress
always reported, never silently discarded — and each head renders it. The
GUI never swallows a degradation notice; MCP never converts partial failure
into silence; refusals remain values, not exceptions, where phase 6 made
them so.

## Testing

- **Extraction**: existing smoke/cucumber suites stay green per chunk;
  byte-identical JSON checks against pre-extraction binaries for each moved
  verb (the phase-6 refactor-proof technique).
- **MCP**: integration tests drive the real `maj mcp` over stdio with an
  rmcp client. Tool-list snapshot (schema drift fails the test); per-tool
  tests including the confirm/dry-run split for every mutating tool;
  search-row parity pinned byte-for-byte against `maj search --json`;
  resource reads return real thumbnail bytes.
- **GUI**: vitest component tests for search flow (debounce, degradation
  display, inspector rendering from fixture outcomes); Rust-side Tauri
  command tests over a fixture catalog; `tsc --noEmit` + oxlint + Tauri
  build in `just ci` for the GUI workspace.
- **Mutation testing** on new service-layer logic per project convention;
  reviewers sabotage-probe (what still passes if this line vanishes?).
- Full WebDriver e2e for the GUI: watchlist, not this phase.

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. `crates/services` skeleton + first verbs (search, get_asset, volumes).
2. Remaining read verbs + saved searches; CLI rethreaded per verb.
3. Mutating verbs (ingest, tag, para, verify, sync, inbox, index,
   catalog_init) extracted with outcome structs.
4. `maj mcp`: server scaffold, read tools, resources.
5. `maj mcp`: mutating tools + confirm semantics + integration suite.
6. Tauri scaffold, GUI workspace split, version-sync check, CI wiring.
7. Search surface + first-run initialize flow.
8. Volumes surface + inspector polish.
9. Release pipeline (tauri-action, updater, cargo-about) + closing
   (handoff, watchlist reconciliation, mutation-testing sweep).

Watchlist items adjacent to this phase (`PortError` opacity, repeated
projection scans in search) are addressed if they block MCP work, otherwise
they stay listed with attribution.

## Deferred (watchlist items with this spec's attribution)

- Browse / Ingest / Organize surfaces; menu-bar indicator with indexing
  throttle; hover-scrub filmstrip.
- GUI WebDriver e2e suite.
- `maj doctor` (natural MCP-era tool; truncated-tail residue check).
- Windows/Linux GUI builds; localization.
- MCP long-running-tool progress notifications (verify_volume/index_run
  stream progress) — land as a follow-up once rmcp progress support is
  assessed during implementation.
