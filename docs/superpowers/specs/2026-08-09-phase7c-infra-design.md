# Majestical Phase 7C — infra: notices-on-error, wire pinning, target-gating

Approved 2026-08-09. Parent spec:
`docs/superpowers/specs/2026-07-28-majestical-design.md`. Phase 7 spec (the
services + MCP + GUI umbrella this phase continues):
`docs/superpowers/specs/2026-08-02-phase7-agent-surface-gui-design.md`.
Prior phase: `docs/superpowers/specs/2026-08-04-phase7b-gui-release-design.md`.

Phase 7C is an infrastructure phase. It closes the two correctness gaps the
7B handoff named first (a failing service call drops its notices; nothing
pins the TypeScript wire layer against Rust) and executes the portability
decision made in this design session: target-gate the Apple-only
dependencies now so the full Rust test suite runs on Linux CI, with
non-Apple OCR/PDF fallbacks deferred. The Browse / Ingest / Organize GUI
surfaces are explicitly **not** in this phase — they are phase 7D, to be
brainstormed separately once this phase's plumbing (notices on error, wire
pinning) is underneath them.

## Scope decisions (from design session)

- **Four workstreams**: notices payload on `ServiceError`; TS wire-layer
  pinning via committed fixtures; target-gating `crates/index`'s Apple-only
  dependencies for a `{macos, ubuntu}` Rust CI matrix; a small CI-hardening
  rider (two zizmor auditor findings the 7B watchlist tagged "7C
  hardening").
- **Decomposition**: 7C = infra, 7D = surfaces. Chosen over one big spec so
  the surfaces can be designed against notices-on-error and wire pinning
  already existing, and over surfaces-first so they need not be retrofitted.
- **Portability goal state**: the whole workspace builds **and its tests
  run** on ubuntu CI. OCR/PDF degrade to absent-with-named-gap on
  non-Apple targets; transcription stays functional (whisper CPU). Windows
  stays out of the matrix — no release artifact exists for it and its
  toolchain friction (protoc, ffmpeg, whisper build) buys nothing yet.
  Non-Apple OCR/PDF *fallback implementations* are deferred to a future
  phase; this phase makes their absence honest, not their presence real.
- **Conformance gates stay macOS-only.** The four model-download gates
  (`just ci`) are unchanged in trigger and platform.

## Architecture

### Workstream A — notices payload on `ServiceError`

Today `crates/services`' sink (`crates/services/src/notices.rs`) is drained
on the `Ok` path only: each head folds notices into the verb's outcome
struct. An `Err` return leaves the sink undrained, so a call that collected
warnings and then failed reports the failure alone. `sync::pull_impl` is
the motivating case: at `PullApplyFailure` its sink holds the buffer
`apply_pulled_events` folded — exactly what is lost.

**The carrier variant**, following the existing partial-progress precedent
(`ServiceError::ParaArchivePartial { moves, source }`):

```rust
/// Wraps any ServiceError that escaped a verb while the notices sink
/// still held diagnostics. Constructed only by App::attach_notices;
/// never nested — attach_notices merges into an existing carrier.
#[error("{source}")]
WithNotices {
    notices: Vec<String>,
    source: Box<ServiceError>,
},
```

**The attachment seam lives inside `crates/services`, not in heads**, so no
head can forget it: a helper on `App`

```rust
fn attach_notices<T>(&self, r: Result<T, ServiceError>) -> Result<T, ServiceError>
```

drains the sink on the `Err` arm and wraps only when the drained vec is
non-empty. Wrapping an error that is already `WithNotices` appends to its
vec rather than nesting. The `Ok` arm passes through untouched — the
existing per-head `Ok`-path folding is not this workstream's to change.
`attach_notices` is applied mechanically at each public verb boundary
(~30 one-line edits; the verb body moves to an inner call where the verb
isn't already a wrapper).

**Head rendering, decided once per head:**

- **CLI**: on `WithNotices`, print each notice line verbatim to stderr
  first (the same rendering the `Ok` path uses today), then render
  `source` as the error is rendered today; exit nonzero. `index_cmd`'s
  end-of-command drain must not double-print notices already rendered from
  the carrier.
- **MCP**: the `isError: true` payload gains the same `notices` field
  successful outcomes carry (via the existing `with_notices` folding in
  `crates/cli/src/mcp_cmd/mod.rs`).
- **GUI**: the serialized `CommandError` (`apps/desktop/src-tauri`) gains a
  `notices: Vec<String>` field (empty omitted, matching the outcome
  structs' skip-if-empty contract); `api.ts`'s `CommandError` interface
  mirrors it; the Svelte views' error paths hand those notices to the
  existing `Notices` component so a failure renders its warnings above the
  error, same as a success renders them above its result.

Exhaustive matches over `ServiceError` in heads break at compile time when
the variant lands. That is deliberate: every head is forced to decide its
rendering rather than inheriting a default.

### Workstream B — TS wire-layer pinning

Nothing today cross-checks `apps/desktop/src/lib/api.ts` (one interface per
outcome struct, snake_case serde names mirrored by hand) against the Rust
structs it transcribes; a renamed serde field breaks the GUI at runtime
with no test failing anywhere. Fix shape (named by the 7B handoff):
Rust-serialized fixtures parsed under the TS types, committed, and pinned
from **both** sides so drift fails a build no matter which side moved.

- **Rust side**: `apps/desktop/src-tauri/tests/wire_fixtures.rs`
  constructs one fully-populated instance of each outcome struct the GUI
  consumes — every field non-default, `notices` non-empty, collections
  with ≥1 element — plus the serialized `CommandError` (with workstream
  A's `notices`), serializes each with the same serde configuration the
  wire uses, and compares byte-for-byte against the committed fixture in
  `apps/desktop/src/lib/fixtures/<name>.json`. A mismatch fails the test
  and names the fixture; `MAJ_UPDATE_FIXTURES=1` rewrites the fixtures
  instead (insta-style regeneration).
- **TS side**: a fixtures module imports each JSON and assigns it to the
  corresponding `api.ts` interface, so a renamed or retyped serde field
  becomes a `tsc`/`svelte-check` compile error. A small vitest suite
  additionally asserts the load-bearing runtime shapes: snake_case field
  presence on a sampled struct, the documented two-depth `get_asset`
  notices, and `CommandError.notices`.
- **Rejected alternatives**: runtime schema validation (zod — new
  dependency, duplicate type definitions) and codegen from Rust
  (ts-rs/specta — the outcome structs' custom `serialize_with` pairs-as-map
  serializers are exactly what derive-based codegen mishandles, and it
  would replace a working hand-written layer wholesale). Revisit codegen
  only if fixtures prove noisy in practice.

### Workstream C — target-gating for a `{macos, ubuntu}` Rust matrix

The Apple-only surface is contained in `crates/index`: `ocr.rs` and
`pdf.rs` (objc2, objc2-foundation, objc2-vision, objc2-pdf-kit,
objc2-app-kit) and whisper-rs's `metal` feature. Everything downstream
inherits macOS-only-ness from these; nothing else in the workspace is
Apple-specific.

- **Workspace `Cargo.toml`**: the objc2 crates move under
  `[target.'cfg(target_os = "macos")'.dependencies]`. `whisper-rs` is
  declared per-target: `features = ["metal"]` on macOS, default features
  (CPU) elsewhere — transcription remains functional on Linux, not gated.
- **`crates/index`**: `ocr.rs` and `pdf.rs` keep their current bodies
  behind `#[cfg(target_os = "macos")]`; each gains a non-macOS sibling with
  identical signatures whose results report through the existing
  degradation path — coverage names the specific gap and remedy ("OCR
  requires the macOS Vision framework; this build has no OCR backend"),
  never a silent zero, per the never-lie invariant. Anywhere capability is
  enumerated (describer/status listings), a non-macOS binary must not
  claim OCR or PDF.
- **CI**: the "Rust checks and tests" job becomes a
  `{macos-latest, ubuntu-latest}` matrix; the ubuntu leg installs protoc
  and ffmpeg. Conformance gates are untouched. The frontend gates were
  already 3-OS and stay so.
- **Not a cargo feature**: features are additive and user-selectable — a
  Linux build with an `apple-native` feature enabled must not be
  representable, so platform selection is `cfg(target_os)`, not a feature
  flag.

### Workstream D — CI hardening rider

Two zizmor auditor-persona informationals recorded on the 7B watchlist:

- The updater signing secrets (`TAURI_SIGNING_PRIVATE_KEY`,
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) move into a GitHub Environment the
  release workflow references. Creating the environment and moving the
  secret values is an operator step (`gh api` or repo settings), documented
  in `docs/RELEASING.md` alongside the existing secret-handling notes.
- The `rust-toolchain` action pin becomes a full SHA with a version
  comment, matching the repo's action-pinning convention.

## Error handling

- `attach_notices` never drops diagnostics: an empty sink wraps nothing; a
  poisoned sink still drains (existing `Notices` behavior, already
  tested); a nested wrap merges rather than shadowing.
- No head renders the same notice twice: the carrier is drained exactly
  once per failure, and the CLI's end-of-command drain paths are audited
  for double-rendering when the error path already carried notices.
- Non-macOS stubs are compile-time honest: they exist only where the real
  implementation cannot, return the same outcome types, and degrade with a
  named gap and remedy — no `unimplemented!`, no silent empty results.
- Fixture drift fails loudly on the side that moved: the Rust test names
  the divergent fixture and the regeneration command; the TS side fails
  type-checking with the field name in the compiler error.

## Testing

TDD throughout; every new guard ships with the test that fails when it is
deleted.

- **Workstream A**: the motivating regression — `sync::pull_impl` failing
  at `PullApplyFailure` surfaces the notices `apply_pulled_events` folded;
  unit tests for `attach_notices` (empty sink, non-empty sink, merge-not-
  nest, poisoned sink); per-head rendering tests (CLI stderr order:
  notices before error; MCP `isError` payload carries `notices`; GUI
  `CommandError` serialization, pinned by a workstream-B fixture); a
  no-double-render test on the CLI path that previously drained at end of
  command.
- **Workstream B**: the Rust fixture test fails when a serde field is
  renamed (verified by mutation during review, per reviewer convention);
  the TS assignment fails `svelte-check` under the same rename; vitest
  runtime assertions on the sampled shapes.
- **Workstream C**: the ubuntu CI leg is itself the test that the gated
  build's suite passes with describers absent; a unit test on the stub
  path asserts the degradation names the gap (exists on the non-macOS
  target — it runs in CI's ubuntu leg, not on developer Macs).
- **Workstream D**: `zizmor` clean at the auditor persona for the two
  addressed findings; `actionlint` clean.
- **Phase close**: scoped `cargo-mutants` runs on `attach_notices` and the
  changed head-rendering seams — foreground, one at a time, each run
  finishing before the next starts (standing mandate).

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. **Chunk 1**: `ServiceError::WithNotices` + `attach_notices` + verb-
   boundary application + CLI/MCP rendering + the `pull_impl` regression
   test.
2. **Chunk 2**: GUI `CommandError.notices` + wire fixtures (Rust test, TS
   assignments, vitest shapes) — the fixture set lands after workstream A
   so `CommandError` is pinned in its final shape.
3. **Chunk 3**: target-gating (Cargo.toml target tables, `ocr.rs`/`pdf.rs`
   stubs) + the ubuntu CI leg.
4. **Chunk 4**: hardening rider + watchlist updates (close the items this
   phase resolves, with PR attribution) + the 7D handoff doc.

## Deferred (watchlist items with this spec's attribution)

- Non-Apple OCR and PDF fallback implementations (this phase makes their
  absence honest; a future phase makes them real).
- Windows in the Rust CI matrix; Windows/Linux release artifacts.
- Browse / Ingest / Organize surfaces, menu-bar indicator, hover-scrub
  filmstrip — phase 7D.
- Codegen of `api.ts` from Rust (revisit only if the fixture set proves
  noisy to maintain).

## As-built (phase 7C)

What shipped, where it differs from the design above. Written as what IS,
not as a change log.

**The attach seam is four verbs, not ~30** (PR #91). The pre-implementation
survey showed every verb except the four sync ones (`status`,
`locations_list`, `push`, `pull`) keeps its sink on `App`/head-side, where
`with_app` in `crates/cli/src/main.rs` already drains on the error path —
only the sync verbs' local sinks drop on `Err`. So `attach_notices` on
`App` never existed; the helper is `Notices::attach_on_err`
(`crates/services/src/notices.rs:55`), applied at exactly those four
boundaries (`crates/services/src/sync.rs:379,419,668,820`) rather than
mechanically at ~30.

**MCP failure notices are leading text content blocks, not a `notices`
field** (PR #91). `rmcp`'s error constructor is
`CallToolResult::error(Vec<ContentBlock>)`; structured content is the
success-path shape, so the `isError: true` result cannot carry the field
the design named. Instead `split_notices` takes the carrier apart and
`error_blocks_with_notices` emits one text block per notice, in push
order, ahead of the inner error's Display chain
(`crates/cli/src/mcp_cmd/mod.rs:87,103`).

**The rust-toolchain finding was `superfluous-actions`, and the fix was
removing the action** (this closing PR). The design read the 7B watchlist
item as a missing SHA pin. zizmor's actual auditor finding was that
`dtolnay/rust-toolchain` duplicates the rustup already on the runner; the
fix is a `rustup toolchain install stable` script step in both workflows,
not a longer pin.

**The ubuntu leg needed two rounds the plan did not anticipate** (PR #93).
First, `clippy::unnecessary_wraps` fires only where the non-macOS
`apply_coreml_ep` stub compiles — the stub keeps `Result` to share the
macOS signature, so the macOS leg never sees the lint; it carries an
`#[expect]` with that reason (`crates/index/src/encoder.rs:203`, PR #93).
Second, four CLI smoke tests exercise Vision/PDFKit end to end
and had to be cfg-gated to macOS
(`crates/cli/tests/index_smoke.rs`, PR #93) — the coverage cost is
recorded on the watchlist under Phase 7C deferrals.

**The parity-reference build step in ci.yml stays macOS-only** (PR #93; the
plan's Task 10 did not call this out). The reference `maj` binary is built
at the merge-base with main, a commit that can predate this phase's Linux
port and so may not compile on the ubuntu leg. The step carries an
`if: runner.os == 'macOS'` guard and a comment saying exactly this; the
`services_parity` suite skips loudly when `/tmp/maj-ref` is absent.

**The environment move is half done, and the outstanding half is operator
work** (this closing PR). The `release` GitHub Environment exists and
`release.yml`'s desktop job declares `environment: release`; that is safe
before the environment holds any secrets because GitHub falls back to the
repository secret of the same name. The two secret VALUES still have to be
moved by the operator (`gh secret set TAURI_SIGNING_PRIVATE_KEY --env
release`, and likewise the password), and the repository-level secrets
stay until a release dry run proves the environment-scoped ones end to
end. `docs/RELEASING.md`'s secrets section is the instruction of record.
