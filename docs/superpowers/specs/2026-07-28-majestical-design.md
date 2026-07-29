# Majestical — Design

Date: 2026-07-28
Status: Approved section-by-section in design session; pending written-spec review.

## What it is

A macOS desktop application for media ingest, cataloging, and offline search, built for
hybrid/remote teams whose media lives on per-site NAS and shuttle drives. It combines
verified camera-card/inbox ingest (OffShoot territory), browse-anything freedom (Kyno),
offline catalogs of disconnected volumes (NeoFinder), and local AI semantic search
(Shade/Peakto territory) — local-first, team-syncable without a server, and agent-native
(full-parity CLI and MCP server).

Organization is opinionated but folksonomy-friendly: PARA (Projects / Areas / Resources /
Archives) is real folder structure applied at ingest time so disks stay legible without
the app; everything else — tags, people, topics — is freeform folksonomy metadata in the
catalog. Existing drives are cataloged as-is, never restructured.

## Requirements (from design session)

1. **Users**: hybrid/remote team; media on per-site NAS plus traveling shuttle drives.
   Async catalog sync; searching volumes you don't have connected is core.
2. **Media**: even mix — video, photos, audio, design files (PSD, PDF, graphics).
3. **AI**: pluggable inference backends (Ollama, LM Studio, OpenRouter), configured per
   catalog. Some catalogs strictly local, others cloud-assisted.
4. **Organization**: PARA on disk at ingest; folksonomy tags in catalog; existing drives
   cataloged without restructuring.
5. **Agent access**: MCP server + CLI with full GUI parity; destructive ops gated.
6. **Stack**: Rust hexagonal core, Tauri UI with auto-update.
7. **Mobile shooters**: cloud-inbox watch-folder ingest day one (HEIC/HEVC/Live Photo
   aware); contribution format designed so a future iOS app targets the same API.
8. **Architecture discipline**: hexagonal (ports and adapters), Gherkin acceptance tests.
9. **Marketing site**: separate sub-project (see Out of scope).

## Competitive research (summaries; full reports in docs/research/)

- **OffShoot (Hedge)** — the ingest benchmark: xxHash64 verification, MHL/ASC-MHL,
  multi-destination, transfer logs. No catalog, search, or tagging. Attackable: weakest
  verification mode is the default; dedupe is name+size+date not content hash; auto-ingest
  is a Pro-only sample script; ASC MHL is paywalled.
- **Kyno** — beloved browse-without-import and Drilldown flat views; metadata in sidecars
  travels with drives. Structural gaps: no offline search of disconnected drives, no
  cross-volume file tracking, proprietary sidecar format, zero AI. Went dormant 4 years
  post-acquisition; a "Kyno + AI search" competitor field now exists.
- **Shade** — proves demand for full-sentence semantic search returning timestamped clips,
  face clustering, transcription. Cloud-captive (pivoted away from local-first);
  $10–15K/yr real-world team cost. Vacated the local-first position.
- **NeoFinder** — 30 years of offline catalogs (catalog-per-volume files with thumbnails),
  XMP write-back as anti-lock-in, smart folders as plain files, team sharing via catalog
  folder on NAS. Weak: slow cataloging, no live sync, no ingest story, dated UI.
- **Peakto** — validates local AI + offline-volume search; compact DB (~3GB/100k images).
  Weak: slow resource-hungry ingest, fixed AI taxonomy brittle on non-mainstream imagery,
  discredited aesthetic scores, missing search affordances (counts, negative operators,
  saved queries), no verified-ingest provenance. Borrow: layered search, background-work
  throttle in menu bar, blue/gray AI-vs-human tag distinction, local-server team access.
- **ASC MHL** — adopt as interchange format (Netflix-recommended; Silverstack/YoYotta/
  ShotPut support). Spec gaps we layer over: no signing, XML doesn't scale, no hero-checksum
  source verification, no cloud story. Python reference implementation is the conformance
  oracle.
- **AI stack** — Ollama and LM Studio cannot produce image embeddings (text-only endpoints);
  OpenRouter alone offers multimodal embeddings. Therefore: in-process image encoder
  (SigLIP 2 / MobileCLIP2 via ONNX Runtime + Core ML EP), LLM backends for captioning/
  classification/query understanding, sqlite-vec for vectors, scene-detect + keyframe +
  faster-whisper + OCR for video, separate indexes merged at query time.

## §1 Architecture

Cargo workspace, hexagonal core, thin adapters. `src-tauri` is deliberately its own
workspace (cuesheet pattern) so GUI deps stay out of headless CI.

```
majestical/
├── crates/
│   ├── core/            # domain model, ports (traits), services
│   ├── catalog-sqlite/  # SQLite + sqlite-vec + FTS5 projection
│   ├── ingest/          # volume scanning, checksummed copy engine, ASC MHL
│   ├── inference/       # Ollama / LM Studio / OpenRouter adapters + in-process embedder
│   ├── sync/            # event-log file sync + content-addressed blob store
│   ├── cli/             # binary: `maj`
│   └── mcp-server/      # binary: MCP over stdio
├── apps/desktop/        # Tauri + Svelte app (own workspace)
├── site/                # marketing site (separate sub-project)
└── docs/
```

Ports defined in `core` (implementations injected):

- `CatalogStore` — persist/query projection (assets, tags, PARA nodes, saved searches)
- `EventLog` — append/read CRDT operation log
- `VolumeSource` — enumerate volumes, walk files, read bytes
- `Embedder` — image/text vectors (in-process)
- `Describer` — captions/classification/transcription via configured backend
- `SyncTransport` — read/write log segments + blobs at a location
- `Clock`, `IdGen` — injected for deterministic HLC/ULID under test

Tauri commands, CLI subcommands, and MCP tools are thin mappings over one core service
layer — full parity by construction.

## §2 Data model

Three layers: physical truth (observed facts, never merged), organizational opinion
(CRDT-merged), derived intelligence (content-addressed, disposable).

**Physical:**

- `Volume` — label, filesystem UUID, hardware serial where readable, capacity, kind
  (internal/external/NAS/card/cloud-inbox), last-seen, last-verified.
- `FileInstance` — path on a volume, size, timestamps, hash history (algorithm, value,
  original/verified/failed action, hashdate), manifest C4s and roothashes per generation.
- `Asset` — logical item keyed by content hash; multiple FileInstances across volumes are
  one asset. Live Photos, RAW+JPEG pairs, sidecars group as one asset with member roles.

**Organizational (CRDT semantics):**

- `ParaNode` — Project/Area/Resource/Archive entry mapped to a real directory. Asset PARA
  assignment: HLC-LWW scalar. Archive = disk move + node retarget, rename graph preserved.
- Tags — OR-Set (add-wins) per asset. Namespaced by convention only (`person/dana`,
  `status/select`). App suggests existing tags to converge vocabulary; never enforces
  schema. AI-suggested tags carry provenance and are visually distinct until confirmed.
- Collections (OR-Sets), ratings/titles (HLC-LWW), notes (whole-field HLC-LWW),
  time-ranged markers on video.

**Merge model:** operation-based CRDT over append-only per-machine event logs. Events carry
ULIDs (idempotent replay) and Hybrid Logical Clock timestamps (causality-respecting order,
clock-skew tolerant). Losing LWW writes remain in the log — recoverable and auditable.
SQLite catalog is a rebuildable projection of the logs.

**Derived (keyed by asset hash + model version):** thumbnails, filmstrips, image/keyframe
embeddings, transcript text + vectors, OCR, EXIF/IPTC/XMP extraction, captions.
Regenerable; model upgrade = incremental re-derive.

**Write-back is deliberate:** explicit "write XMP" operation exports confirmed metadata to
sidecars/embedded XMP (NeoFinder anti-lock-in) — never automatic (Peakto's Lightroom
conflict lesson).

## §3 Ingest pipeline

Flow: detect → plan → copy+hash → verify → manifest → catalog → file into PARA.

- **Sources**: mounted volumes/cards (polling fallback for macOS's unreliable card-mount
  events), any folder, cloud inbox. Auto-ingest rules (volume-name pattern → preset) are
  built-in, not scripts.
- **Verification safe by default**: xxHash64 on source read + destination read-back.
  Faster modes opt-in with warnings. 0-byte detection, missing-file check at end.
- **Content-hash dedupe** pre-copy: skip / copy-anyway / copy-and-link.
- **Multi-destination**, each independently verified with its own ASC MHL history.
  Cascading is a later phase.
- **ASC MHL standard tier** (not paywalled): spec-compliant `ascmhl/` per destination;
  re-verification appends generations; catalog stores manifest C4s independently to detect
  tampering. Conformance-tested against the Python reference implementation. Phase order:
  create + verify first; nested histories, rename tracking, flatten/collections later.
- **PARA routing**: ingest targets a ParaNode (`Projects/<slug>/`) with a token template
  for inner layout (`{date}/{source-label}/…`). Batch tags at ingest time.
- **Engine**: parallel readers/hashers/writers, bounded queues, checkpointed transfer
  journal (resumable across app restart), background throttle surfaced in UI.
- **Inbox**: HEIC/HEVC aware; Live Photo pairs grouped; see §5 for contribution format.

## §4 AI indexing & semantic search

- **In-process embedder (always local, bundled)**: SigLIP 2 B/16 (768-d) via ONNX
  Runtime with Core ML execution provider (image tower on ANE, text tower on CPU — the
  proven split; watch the preprocessing pitfalls documented in docs/research/ai-stack.md).
  MobileCLIP2 is the documented lighter alternative if model size becomes a constraint.
  Model id+version stored per vector.
- **Describer backends (per-catalog)**: Ollama / LM Studio / OpenRouter behind one
  OpenAI-compatible adapter with three profiles. Used for captions, open-vocabulary tag
  suggestion, classification into the catalog's existing folksonomy, query understanding.
  OpenRouter opt-in additionally unlocks hosted multimodal embeddings as a quality tier.
- **Video**: ffprobe → scene detection (2s min scene length; uniform-sampling fallback;
  ~150 frame cap) → keyframe embeddings with (asset, timestamp) → local faster-whisper
  transcription with timecodes → per-scene OCR. Audio: transcription. PDFs/design files:
  text extraction + preview embedding.
- **Storage**: sqlite-vec + FTS5 in catalog SQLite. Documented migration path to LanceDB
  at scale; not a day-one dependency.
- **Query model**: layered — semantic (visual + transcript indexes kept separate, merged
  at query time), keyword FTS (names/captions/OCR/transcripts), hard filters (volume,
  PARA, tags, dates, codec, online/offline). Result counts, negative operators
  (`-tag:rejected`), saved searches (synced via CRDT).
- **Background queue** with visible progress + throttle. Priority: thumbnails →
  embeddings → transcription → captions. Filename/metadata search available immediately
  post-ingest. Derived blobs sync to teammates; disconnected volumes stay searchable.

## §5 Sync, teams, mobile contribution

Sync = files at one or more **sync locations** (NAS share, Dropbox/iCloud folder, or a
shuttle drive itself):

```
<sync-root>/
├── events/<machine-id>/NNNN.jsonl   # append-only op logs (ULID, HLC)
└── blobs/<hash-prefix>/<hash>       # derived data, content-addressed, zstd
```

Push own segments, pull others', merge idempotently. Shuttle drive carrying its own
sync-root converges both sites on plug-in. Blob sync lazy + prioritized (thumbnails
first) so teammates browse/search volumes they never mounted. Events carry author
identity; read-only members simply never push.

**Mobile contribution**: a contribution = folder of media + `contribution.json`
(contributor, capture context, client-computed xxHash64s, intended PARA target). Inbox
watcher validates hashes, runs verified ingest, files into PARA, tags provenance
(`contributor/dana`, `source/iphone`). Day one: shared iCloud/Dropbox folder + share-sheet
Shortcut generates the manifest. Future iOS app and future self-hosted server (local
Frame.io pattern) target the same format. Manifest-less drops still ingest — hashed on
arrival, filed to a triage node.

## §6 Agent surface & GUI

**CLI (`maj`)** — JSON-first (`--json` everywhere): `search`, `ingest`, `tag add|rm`,
`para add|move|archive`, `volumes list`, `verify`, `sync push|pull|status`,
`index status|throttle`, `catalog init|join`. Destructive ops default `--dry-run`;
`--yes` executes.

**MCP server (`maj mcp`)** — stdio; tools mirror CLI verbs (`search_assets`,
`ingest_source`, `tag_assets`, `move_para`, `verify_volume`, `get_asset`), thumbnail/
keyframe resources so agents can see results, stable asset IDs + timestamps for chaining.
Mutating tools take `confirm` defaulting to dry-run diff.

**GUI (Tauri + Svelte)** — five surfaces: Search (omnibox, counts, timestamped video
hits, saved searches), Browse (Drilldown flat grid, filmstrip hover-scrub), Ingest
(sources/destinations board, queue), Organize (PARA tree, tag manager with merge/rename
as CRDT events), Volumes (the shelf: online/offline, last verified). Menu-bar indicator
with indexing throttle.

## §7 Error handling, testing, delivery

**Error handling** — never lie about data safety:

- `thiserror` typed errors carrying operation, input, suggested fix. No swallowed errors.
- Verification failure → files marked failed in manifest + catalog, partials quarantined,
  re-copy offered. Transfer journal makes crash recovery a resume.
- Backend down → indexing stage pauses visibly, retries with backoff; search degrades to
  what's indexed, never errors.
- Corrupted log segment skipped + reported; corrupted catalog rebuilds from logs.

**Testing:**

- Gherkin acceptance tests (cucumber-rs) at the hexagon boundary with fake
  `VolumeSource`/`Clock`/`Embedder`, real SQLite in temp dirs.
- proptest for algebraic correctness: CRDT merge (commutativity, associativity,
  idempotence), hash pipeline, path routing.
- ASC MHL conformance: our output verified by the Python reference `ascmhl verify` in CI;
  golden-file manifest tests.
- cargo-mutants on CRDT + verification modules. CLI e2e smoke suite. `just ci` = CI.

**CI/CD & distribution** (cuesheet patterns + its gaps closed):

- SHA-pinned actions with version comments, `persist-credentials: false`, per-job
  permissions, split headless/GUI workspaces, no-cache release builds,
  degrade-gracefully-without-secrets signing, tag → tauri-action draft release +
  `latest.json` → cargo-about license bundle → human publishes.
- Auto-update armed from day one: updater key generated, `pubkey` set, update check wired
  into the app shell. Endpoint: `releases/latest/download/latest.json`.
- Added over cuesheet: zizmor + actionlint in CI, Dependabot (7-day cooldown, grouped),
  prek hooks, version-sync check across Cargo.toml/tauri.conf.json/package.json, clippy
  lint table declared in Cargo.toml.

## Out of scope (this spec)

- Marketing site content/design — separate sub-project (same Pages deployment pattern).
- iOS companion app and self-hosted sync/collaboration server — the contribution format
  and sync-location abstraction are their integration points.
- Cascading transfers, LanceDB migration, MV-register conflict chips, nested ASC MHL
  histories/flatten — explicitly later phases.
- Editing/develop features (not a Lightroom competitor); cloud storage tiers.

## Build order (suggested for planning)

1. Core domain + event log/CRDT + SQLite projection (acceptance-test harness first)
2. Volume cataloging + offline search (no AI) — the NeoFinder baseline
3. Ingest engine + ASC MHL create/verify — the OffShoot baseline
4. In-process embeddings + layered search — the differentiator
5. Describer backends (Ollama/LM Studio/OpenRouter) + video pipeline
6. Sync + inbox contributions
7. CLI/MCP polish, GUI surfaces, release pipeline (scaffolded from day one)
