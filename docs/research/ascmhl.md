# ASC MHL — Research Findings

## 1. What it is / vs. legacy MHL

ASC MHL is the ASC Motion Imaging Technology Council (Advanced Data Management Subcommittee) standard for media chain of custody. **Spec v1.0 dated 15 March 2022** ([PDF](https://cdn.theasc.com/ASCMHL_Specification_v1.0.pdf), [repo](https://github.com/ascmitc/mhl-specification)). Internally it is **MHL version 2.0** — the `hashlist` XML `version` attribute is literally `"2.0"` to distinguish it from Pomfort's legacy MHL 1.1 ([mediahashlist.org](https://mediahashlist.org/), still valid and separately maintained).

Legacy MHL = one standalone XML snapshot per copy operation. Multiple MHLs on a drive are mutually unaware and can silently contradict each other; no metadata, no creator info, no linkage. ASC MHL adds: linked generations, nested histories, directory hashes, ignore patterns, rename tracking, arbitrary custom metadata, and a chain file that hashes the manifests themselves. Hedge's writeup notes ASC MHL essentially standardizes the out-of-spec "MHL Awareness" they had already shipped ([hedge.co/blog/asc-mhl](https://hedge.co/blog/asc-mhl)).

## 2. Core concepts

- **Manifest** — one `.mhl` XML file, immutable once written, covering a scope (its root directory). Contains `creatorinfo` (creationdate, hostname, tool+version, optional author w/ email/phone/role, location, comment), `processinfo` (process type, roothash, ignore patterns), `hashes`, optional `references`, optional `metadata`.
- **History** — the `ascmhl/` directory at the root of a media directory: chain file + all manifests it references. Travels automatically with the media when copied. May contain optional `README.txt` (not part of the history).
- **Chain** — `ascmhl_chain.xml`, table of contents, one `hashlist` entry per manifest with `sequencenr`, relative `path`, and a **C4 hash of the manifest file** — the only tamper-evidence mechanism.
- **Collection** — `ascmhl_collection.xml`, same schema as chain but manifests may have independent scopes. Used as a packing list / receipt, typically over flattened manifests (e.g. email one file covering several camera cards).
- **Hash formats** — MD5, SHA1, C4 (SMPTE ST 2114, SHA-512 in Base58, `c4`-prefixed, 90 chars), XXH64, XXH3, XXH128. xxHash uses **seed 0, big-endian**. No SHA256 in v1.0 (a Frame.io article claims otherwise — wrong).
- **Hash action attribute** — every file hash is labeled `original`, `verified`, or `failed`. Only `original`/`verified` may be used for later verification; `failed` is permanently poisoned. v1.0 explicitly has **no rehabilitation path** for an intentionally-changed file.
- **Directory hashes** — each directory gets a `content` hash (hash-of-hashes over immediate children's file hashes and children's content hashes) and a `structure` hash (same but each child's name is concatenated with its hash first, so renames/moves change it). Hash-of-hashes = sort hash list lexicographically, write raw bytes into one generator, digest (Appendix G). A `roothash` in `processinfo` represents the whole data set.
- **Signing** — **none**. There is no digital signature, PKI, or cryptographic attestation anywhere in the spec. Authorship is self-asserted free text. Integrity of the record itself rests solely on the chain file's C4 hashes, and those are trivially rewritable by anyone with write access to `ascmhl/`.

## 3. Structure and generations

Manifest naming: `NNNN_<foldername>_YYYY-MM-DD_HHMMSSZ.mhl` (4+ digit sequence starting at 1, UTC). All manifests created by one logical operation share the same timestamp even across nested histories. Paths are relative, forward-slash, case- and whitespace-preserving, no `..`.

Histories **nest**: a hash record lives in the history closest to the file in the tree. Higher-level manifests reference nested manifests via `hashlistreference` (path + C4). Updates propagate downward (verifying a root history writes new generations in every nested history) but never upward — so a nested update leaves the parent's references stale by design.

Partial copies and re-verifications both become new generations: append-only. A verify writes a generation containing just the verified subset; adding files writes a generation containing just the new files. Consequence: **a file's full record is scattered across many manifests**, and a reader must parse the entire chain (plus nested chains, plus `previousPath` rename links) to assemble state.

Operations: Create, Diff (no hashing — existence only), Verify, History Append, Rename (records `previousPath`, recorded once and not repeated), Flatten (consolidates to a standalone manifest, drops directory hashes and `failed` hashes, keeps earliest hash per algorithm, `process` = `flatten`).

Ignore patterns are gitignore syntax, recorded in the manifest. Default set: `.DS_Store` and any `ascmhl` directory.

## 4. Reference implementation

[github.com/ascmitc/mhl](https://github.com/ascmitc/mhl) — **Python, MIT license**, ~76 stars, primarily authored by Pomfort (`opensource@pomfort.com`). `mhllib` library + `ascmhl` and `ascmhl-debug` CLIs. Python ≥3.11; deps: Click, lxml, pathspec, xxhash.

Maturity: **modest but alive**. v1.0.1 Mar 2024 → v1.0.4 Aug 2024 → v1.1 Dec 2024 (Windows support) → **v1.2, 4 July 2025** (ignore-pattern fixes, better nested flattening, large-history perf). Last commit at time of writing is that release. ~3,200 PyPI downloads/month, ~30 open issues.

Commands: `create` (with `-sf` single-file, `-dr` rename detection, `-n` skip directory hashes, ignore + creator-info options), `diff`, `flatten`, `info` (`-sf` per-file history; `-s`/`-l`/`-dh` still **not implemented**); debug tool adds `verify` (`-sf`, `-dh`, `-pl` packing list "TBD"), `xsd-schema-check`, `hash`. README's own "Known issues" admits it is not a complete implementation and notably that **the chain file is not verified**.

## 5. Industry adoption

- **Netflix** — ASC MHL is the *recommended* manifest format for OCF/OPA ([Production Assets: Data Management](https://partnerhelp.netflixstudios.com/hc/en-us/articles/360000581207-Production-Assets-Data-Management)) and effectively *required* for Footage Ingest uploads: one manifest per camera/sound roll, `ascmhl` folder at the root of each roll directory (`Camera_Roll/A001/ascmhl/A001.mhl`), reel name inferred from the folder above the manifest, checksums limited to xxHash64be/xxHash128/MD5, and **files/folders/MHLs must not be modified after offload**. Netflix re-verifies on upload and again after cloud archive.
- **Pomfort Silverstack / Silverstack Lab / Offload Manager / MediaVerify** — support since Silverstack 8.4; **default manifest format since 9.0**; sealing uses ASC MHL as of 9.0 and can emit flattened manifests for email.
- **Hedge OffShoot Pro** — ASC MHL is a paid Pro-tier feature, limited to Archive transfer mode.
- **YoYotta v4** — creates and parses ASC MHL, browses chain history.
- **Imagine Products ShotPut Pro 2022+ / myLTO / TrueCheck** — co-developed the guidelines.
- **RED** — in-camera ASC MHL generation on some models (per Pomfort).
- **ARRI** documents ASC MHL v2 in its data-transfer guidance.

## 6. Design lessons for a Rust ingest/catalog tool

**Handles well:** immutable append-only generations; the `ascmhl/` folder riding along with the media so custody survives dumb copies; per-hash `original`/`verified`/`failed` provenance; gitignore-syntax ignore patterns recorded *in* the manifest (so the receiving tool applies the same exclusions); directory content+structure hashes enabling a whole-tree check without rehashing every file; custom `metadata` elements at both hashlist and per-file level; the collection/flatten path for emailing a small verifiable receipt.

**Known pain points (mostly from Hedge's critique, plus Pomfort's own caveats):**
- **XML does not scale.** Fine for a camera roll; unusable for millions of files on a 20 TB LTO or VFX/stills workflows. Expect to hold your own index and treat MHL as an interchange format, not your database.
- **No cloud/object-storage story.** S3 multipart checksums are checksum-of-checksums; verifying an ASC MHL against a bucket means egressing every byte. Nothing in the spec addresses this.
- **No source verification.** The spec only defines in-place vs. transfer; two-independent-read source verification (the "hero checksum") isn't expressible, and the in-place workaround forces a read-all-then-copy-all pattern incompatible with file-by-file transfer.
- **Only the most recent checksum is verified**, not the original hero checksum.
- **Nested-history reference staleness** is baked in (updates don't propagate upward).
- **Silverstack does not write directory hashes**, which makes empty folders read as "new" downstream — so don't assume directory hashes exist.
- Hash format must match to continue a history; renaming-on-offload can't continue an existing history.
- Hedge calls the spec "convoluted... pretty costly to implement" — a real signal for scoping a Rust implementation.
- **No signing.** If chain-of-custody credibility matters, you must layer your own attestation on top; ASC MHL alone is tamper-*evident* against accidental corruption, not against a motivated actor.

**What a catalog should persist for later re-verification of offline drives** — the per-file record (relative path, size, creation + last-modification dates, every hash with algorithm, action label, and `hashdate`), the full `previousPath` rename graph, per-generation `creatorinfo` and `processinfo` (tool+version, hostname, author, location, comment, process type — this is the actual custody evidence), the ignore patterns in force per generation, directory content/structure hashes and the roothash per generation, the chain's sequence numbers and manifest C4 hashes (so you can detect a doctored `ascmhl` folder that the reference tool won't catch), the nesting graph, and a volume/media identity (label, UUID, serial, LTO barcode) — the spec gives you none of that, and it's exactly what you need to answer "which shelf is this file on, and when was it last proven good?" Storing the raw manifest bytes alongside the parsed rows is cheap and lets you re-derive anything the parser missed.
