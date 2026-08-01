# Majestical Phase 6 — Multi-location sync + inbox contributions

Parent spec §5 (sync, teams, mobile contribution) and §6 (`sync push|pull|status`),
build-order step 6. First phase where a second machine exists. Everything phases
1-5 built was shaped for this: events are immutable and commutative, blobs are
derivation-keyed and idempotent, SQLite and Lance are per-machine disposable
projections. This phase adds no new op variants and no wire-format change —
sync moves files; it does not create events — and that absence is asserted,
as in phase 5.

## Scope decisions (from design session)

- **One spec, two parts, sync first**: multi-location sync lands and merges
  before inbox work starts. They share almost nothing structurally (inbox
  rides `crates/ingest`).
- **Git-remote model**: the catalog root stays the machine's local,
  always-available sync root. Sync locations are configured remotes (NAS
  path, Dropbox/iCloud folder, shuttle drive), each holding the same
  `events/` + `blobs/` layout. Transports stay dumb.
- **Stateless diff-as-queue**: every push/pull plans by diffing local vs
  remote trees directly — no per-location sync-state files, no cached
  claims. Same shape as `index run`; an interrupted sync converges next run
  by construction.
- **All blobs, priority-ordered**: sync transfers every derivation blob,
  ordered thumbnails → small JSON → vectors → transcripts. "Lazy" =
  ordering + resumability, not exclusion. `--only` covers constrained
  transports.
- **No `SyncTransport` port yet**: every phase-6 location is a mounted path
  on macOS. The engine is written against plain paths; the port arrives with
  the first non-filesystem transport (deferred, watchlist).
- **Inbox is a CLI command, no daemon**: `maj inbox process` runs one
  converging pass; cron/launchd/agents supply recurrence. A resident watcher
  waits for the GUI phase.
- **Manifest format + processing only**: the share-sheet Shortcut that
  generates `contribution.json` on a phone is a follow-up. The schema is
  versioned and documented as the integration point for the Shortcut and the
  future iOS app.
- **Post-ingest disposition**: successful contributions move (atomic rename)
  to `<inbox>/.processed/`; `--keep` leaves them in place. Hash-mismatch
  failures stay untouched in the inbox and are recorded per-machine.

## Architecture

### Sync model — set-union, both directions

Push = local→remote union. Pull = remote→local union. Sync never deletes and
never truncates, in either direction.

Push replicates **everything the local root has** — this machine's own
segments, segments previously pulled from other machines, and all blobs —
not just its own segments. That gossip generalization is what makes a
shuttle drive converge two sites that are never online together: site A's
machines write through it to site B and vice versa. "Push own segments,
pull others'" (parent spec) is the degenerate two-machine case.

**Segments — longer-wins, whole-file, atomic.** A machine's segment is
append-only with exactly one appender (the owning machine), so any shorter
copy is a strict prefix of a longer one. Transfer rule per segment file:

- Destination missing the file, or destination shorter → write the full
  local copy via temp-file + rename (atomic on the same filesystem; temp
  files live under the destination's own tree so rename never crosses
  filesystems).
- Destination equal or longer → skip.

A race between two concurrent pushers of the same segment ends with one
complete valid file (rename is atomic); if the shorter one wins, the next
sync restores the tail, and event replay is ULID-idempotent, so convergence
survives every interleaving. Equal-length-but-different-bytes divergence
(a machine-id reused after a reinstall) violates the single-appender
invariant and is **not detected this phase** — documented, watchlist.

**Blobs — presence-union by derivation key.** Blob files are immutable and
idempotently keyed (asset hash + derivation kind + model tag), so the diff
is a path-presence walk. A destination file whose size differs from the
source is a torn copy from some non-atomic tool and is re-copied via
temp + rename. Transfer order is the priority ladder:

1. `thumbs` — `Thumb`
2. `metadata` — the small JSON: captions, OCR, tags, keyframe manifests,
   completion markers
3. `vectors` — image/keyframe embeddings, transcript chunk embeddings
4. `transcripts`

(the class names are exactly the `--only` values, plus `segments` for the
event log itself)

so an interrupted first sync already leaves a teammate with a browsable
catalog.

### Segment rotation (`crates/sync`)

`append` rotates to the next zero-padded `NNNN.jsonl` when the current
segment would exceed **4 MiB**, bounding push's whole-file re-copy cost.
Readers already merge all `*.jsonl` sorted; `read_since_reporting` already
gives unknown segments a fresh cursor from offset 0. Names stay zero-padded
equal-width (the documented lexicographic-order constraint); overflow past
`9999.jsonl` is a hard error naming the machine directory.

### Sync-crate cleanups (watchlist items, done while in there)

- Unify `read_all_reporting` with the `read_since_reporting` walk:
  read-all becomes read-since-from-zero (discarding cursors), fixing the
  divergence where one bad byte failed a whole segment in the read-all path
  while the incremental path degraded per line.
- `LogError::io(path, source)` constructor replaces the 13 hand-built
  `map_err` closures.
- Sync orchestration gets its own error enum in the same thiserror style;
  messages carry path + operation + remedy.

### Location config — per-machine `sync.toml` (state dir, never synced)

Mount points differ per machine, so locations are per-machine config in the
state dir, next to `describer.toml`:

```toml
readonly = false          # true → `maj sync push` refuses, naming this file

[[location]]
name = "nas"
path = "/Volumes/Team/majestical-sync"

[[location]]
name = "shuttle"
path = "/Volumes/Shuttle/majestical-sync"
```

`readonly = true` is the entire read-only-member feature: events already
carry author identity; a reader who never pushes needs no new data concept.

### Inbox contributions

**`contribution.json`** — versioned, at the root of a contribution folder,
documented as a reference page (the Shortcut and future iOS app target it):

```json
{
  "version": 1,
  "contributor": "dana",
  "para_target": "Projects/spring-campaign",
  "source": "iphone",
  "note": "free-form capture context",
  "files": [
    { "name": "IMG_0421.HEIC", "xxh64": "1a2b3c4d5e6f7a8b", "size": 2048123 }
  ]
}
```

`version`, `contributor`, `files` (relative name + client-computed xxHash64
hex + byte size) required; `para_target`, `source`, `note` optional. Unknown
`version` → contribution skipped with a notice naming the found and
supported versions — never a guess.

**`maj inbox process <path>`** — one converging pass over the inbox folder:

- **Subfolder containing `contribution.json`** → validate: every listed
  file present with matching size; files present but not listed are
  reported and left untouched — never silently absorbed into a manifested
  contribution, and never routed to triage from inside one. Missing/short
  files =
  "still uploading": skip with per-file notice; the next pass converges.
  Complete → run the existing verified multi-destination ingest into the
  manifest's PARA target; ingest's source-read xxHash64 is compared against
  the manifest hash (end-to-end, phone-to-catalog verification); tag
  `contributor/<name>` and, when present, `source/<x>` — all plain `TagAdd`
  / existing PARA ops. A contribution is atomic: any hash mismatch fails
  the whole contribution and nothing from it is ingested.
- **Manifest-less drops** (bare files, or folders without a manifest) →
  eligible only after quiescence (no contained mtime younger than 5
  minutes, so mid-upload files are never grabbed); ingest to the PARA node
  given by `--triage-target` (required if any manifest-less items exist —
  no silently invented default), tagged `source/inbox`, hashed on arrival
  by normal ingest, no contributor identity claimed.
- `para_target` naming a nonexistent PARA node → that contribution fails
  with the node name and `maj para add` as the remedy.
- **Success** → contribution folder atomically renamed to
  `<inbox>/.processed/<folder>` (shared-folder sync propagates the move
  back to the contributor as the "received" signal; the next pass skips it
  for free). `--keep` disables the move. A name collision under
  `.processed/` gets a numeric suffix rather than failing.
- **Hash mismatch** → contribution stays untouched in the inbox; the
  failure (folder, file, expected vs computed hash) is recorded in a
  per-machine `inbox-failures.json` in the state dir (same pattern as
  `index-failures.json`) so later passes skip it with a notice instead of
  re-hashing forever; the report names the fix (re-upload, or remove the
  folder). A changed manifest or changed file mtime/size clears the marker
  and re-validates.

## Commands

All `--json`, like the rest of the CLI.

- `maj sync location add <name> <path>` — validates reachability,
  idempotently initializes the `events/` + `blobs/` skeleton at the path
  (git-init style). `list`, `rm <name>` (`rm` edits config only; never
  touches the location's files).
- `maj sync push [--location <name>] [--only segments|thumbs|metadata|vectors|transcripts]`
  — all configured locations by default. Refuses under `readonly = true`,
  naming the setting and file. Reports per location: segments written
  (files/bytes), blobs written per class, up-to-date counts, skipped
  locations with reasons.
- `maj sync pull [--location <name>] [--only …]` — mirror image. Ends by
  running the existing incremental apply and reporting it
  (`applied 214 new events from 2 machines`), then a remedy notice for
  fetched derived data
  (`fetched 1,830 blobs; run 'maj index run' to make fetched vectors and
  text searchable`). Pull does **not** auto-run indexing — commands stay
  composable; agents chain them.
- `maj sync status` — per location: reachable or not, segments
  ahead/behind per machine (files and bytes), blobs missing each way per
  derivation kind. Every count comes from walking real files at that
  moment. An unreachable location is a reported row, not an error.
- `maj inbox process <path> [--triage-target <para>] [--keep] [--json]` —
  per-contribution outcomes: ingested (with counts + destination), skipped
  (with reason: uploading, unknown version, recorded failure), failed
  (with remedy).

Partial failure policy: push/pull collect per-location results and keep
going; exit code is nonzero only when every requested location fails.
Unreachable (unmounted NAS, ejected shuttle) is a skip-with-notice naming
the path, not an abort.

`maj index status`'s per-derivation coverage already counts real rows and
blobs, so "teammate derived it, you haven't pulled it" surfaces there
naturally once pull lands blobs — with `maj sync pull` as the named remedy
in search degradation notices where a text source's coverage gap traces to
unpulled blobs.

## Error handling

- Nothing on any failure path deletes, truncates, or moves shared data.
  The only move sync/inbox ever performs in shared space is the
  `.processed/` rename on contribution success.
- Every skip carries its reason and remedy: unreachable path, `readonly`
  flag + file, unknown manifest version, still-uploading file (which file,
  expected vs present size), recorded hash-mismatch (which file, both
  hashes, the fix), missing PARA node (+ `maj para add`).
- Temp files from interrupted syncs live under a `tmp/` sibling inside the
  destination tree, are ignored by all readers (not `.jsonl`, not at blob
  paths), and are cleaned opportunistically on the next sync to that
  location.
- `LogError` gains the `io()` constructor; sync orchestration and inbox get
  their own thiserror enums with path + operation + suggested fix.

## Testing

Acceptance criteria are the existing CRDT properties — reused, not
reinvented:

- **Convergence property test**: N machines, random interleavings of
  append / push / pull across random locations, then a final sync round —
  all machines hold equal event sets and equal projections (the proven
  commutative/idempotent apply is the oracle). Blob presence-union
  converges identically.
- **Shuttle e2e**: site A (two machines + a NAS location dir), site B (one
  machine), one shuttle dir carried between them; both sites converge
  without ever sharing a live location.
- **Sabotage probes** (every assertion must be able to fail):
  - delete a remote blob after push → `status` reports it missing (counts
    are walked, not cached);
  - truncate a remote segment externally → push restores the full file
    (longer-wins is live);
  - kill a push mid-copy leaving temp files → next run converges and no
    torn segment is ever visible to readers.
- **Rotation**: appends past 4 MiB rotate to the next segment; readers
  merge, cursors advance, rotated segments transfer once (equal-size skip);
  `9999` overflow errors clearly.
- **Priority order**: the transfer plan/log is asserted ordered
  thumbnails → JSON → vectors → transcripts.
- **Read-path unification**: the corrupt-byte fixture that previously
  failed the whole segment through `read_all` now degrades per line on
  both paths.
- **Inbox**: cucumber feature for contribution flows
  (`crates/cli/tests/features/inbox.feature`); unit coverage for manifest
  parse/validate, unknown version, quiescence, short-file skip, unlisted
  files, hash-mismatch marker write/clear; e2e through real verified
  ingest asserting MHL generation, provenance tags, the `.processed/`
  move, `--keep`, and content-hash dedupe on a re-dropped contribution.
- **Wire format**: assert phase 6 added zero op variants.
- cargo-mutants triage per house convention.

## Delivery — chunked PRs (1-2 tasks each, squash-merge after green CI)

1. Sync-crate cleanups: unified read walk, `LogError::io`, rotation +
   overflow error, rotation tests.
2. Location config + `maj sync location add|list|rm`.
3. Push engine: segment longer-wins + blob presence diff, priority order,
   temp+rename atomicity, `readonly` refusal.
4. Pull engine + incremental apply on completion + remedy notices.
5. `maj sync status` + convergence property test + shuttle e2e + sabotage
   probes.
6. `contribution.json` schema + validation + `maj inbox process`
   (manifested flow, `.processed/`, failure markers).
7. Manifest-less triage flow + quiescence + inbox cucumber + e2e.
8. Closing: wire-format assertion, cargo-mutants triage, handoff.

## Deferred (watchlist items with this spec's attribution)

- `SyncTransport` port — arrives with the first non-filesystem transport
  (self-hosted server / iOS app integration point).
- Divergence detection within one machine's segments (equal length,
  different bytes — reused machine-id after reinstall).
- Share-sheet Shortcut that generates `contribution.json` on-device.
- Resident inbox watcher (FSEvents) — GUI phase.
- Auto-import of pulled blobs into Lance/`text_fts` as part of pull —
  deliberately left to `maj index run` for composability; revisit if the
  two-step trips real users.
