## Peakto (CYME) — Research Findings

### 1. Purpose, target user, federation model

Peakto is a macOS-only "meta-cataloger" — it does not replace your DAM, it indexes the DAMs you already have. Target user: photographers/videographers with 100k–500k+ assets fragmented across a decade of app migrations (Aperture → Lightroom → Capture One → …) and across many drives. ([cyme.io/products/peakto](https://cyme.io/en/products/peakto/))

Federation is **read-only native parsing of foreign catalog formats**, not scripting or plugins. Peakto opens `.lrcat`, `.cocatalog`/`.cosessiondb`, Apple Photos libraries, Aperture, Luminar, iView `.ivc`, DxO, ON1, Topaz, plus Premiere Pro 2024/2025, Final Cut Pro v11, iMovie projects and plain folders. Critically, **the source app need not be installed or even runnable** — this is the killer feature for dead apps like Aperture and iView. It builds its own SQLite-ish database plus small thumbnails, and pulls higher-res previews *from inside the source catalog on demand* rather than transcoding its own. It never copies masters. ([Peakto Documentation PDF](https://d1bb06sh1koghb.cloudfront.net/Peakto/Resources/documentation/v200/PeaktoDocumentation.pdf), [comparison page](https://cyme.io/en/products/peakto/comparison/))

Sync back to sources: "Deep Sync" reparses a catalog for adds/deletes/keyword/album changes; incremental checks run in background (every ~15 min in the Peakto Search plugin variant). ([CYME KB](https://desk.cyme.io/portal/en/kb/articles/all-the-commands-explained-in-the-source-menu))

### 2. AI features — all local

Everything runs on-device; CYME markets "no cloud, nothing uploaded" as the core differentiator. Optimized for Apple Silicon. **CYME does not publish which models/frameworks it uses** — no public statement of CLIP/Core ML/Whisper specifics was found; the CTO is Matthieu Kopp ([MacVoices NAB 2026](https://macvoices.com/macvoices-26163-nab-cyme-expands-peakto-for-larger-libraries-and-team-collaboration/)).

Capabilities: conversational/natural-language search over image *and* video frames (multi-language, but **offline search is English-only** — implying a language model component gated by connectivity, [video-frame-search FAQ](https://cyme.io/en/products/peakto/features/video-frame-search/)); reverse-image/similarity search with an adjustable tolerance slider (Close/Standard/Tolerant); auto-categorization into fixed taxonomies (Panorama view: Architecture, Astro, Wildlife, Automotive, Events & Wedding, Fashion, Nature, Portrait, Water/Underwater; styles; color harmonies — analogous, split-complementary, triad, square); aesthetic score + technical score + brightness/saturation/colorfulness; AI keyword suggestion (rendered blue vs. gray for source-catalog keywords); face detection/clustering, and it *imports* existing face annotations from Apple Photos and Lightroom.

**2.6 (Jan 2026)** added library-wide deduplication and AI culling — clusters near-duplicates across all sources at once (rules: RAW vs JPEG, size, rating), culling templates with reorderable criteria (eyes open, smiles, face quality). Non-destructive; user confirms. Also "Instants," automatic grouping of versions of the same shot. ([PetaPixel](https://petapixel.com/2026/01/14/peakto-2-6-tracks-down-all-your-duplicate-photos-no-matter-where-they-are/), [dedup feature page](https://cyme.io/en/products/peakto/features/find-duplicate-photos/))

### 3. Video and audio

Full video pipeline: proxy generation for scrubbing, contact sheets, frame-level semantic search with blue-bar hit markers on the timeline, subclips and markers, bins exported as FCP7/FCPX XML or sent as timelines to Premiere/DaVinci/FCP. **Automatic local speech transcription** of every clip, searchable as text with jump-to-timecode. Recent format adds: Nikon NEV, RED R3D, Blackmagic RAW. A Premiere Pro plugin launched at Adobe MAX Oct 2025. ([audio-transcript](https://cyme.io/en/products/peakto/features/audio-transcript/), [videographers page](https://cyme.io/en/products/peakto/madefor/videographers/))

### 4. Organization model and XMP

Virtual by default — albums and smart albums span sources without moving files; source folder hierarchy is mirrored, not replaced. Write-back exists but is **narrow**: XMP roundtrip (sidecars, plus optional embedding for JPEG) is *restricted to Watched Folders* only, configured in Preferences → Sync. Commands "Update Pending Metadata" / "Update All Metadata" push annotations out. Real-time bidirectional keyword sync with Lightroom/Capture One arrived in 2.2. But **ratings/flags set in Peakto do not propagate back to Lightroom** — a top complaint ([Greg Benz](https://gregbenzphotography.com/photography-reviews/find-anything-in-lightroom-with-peakto-ai-search/)), and Adobe forum users report the XMP roundtrip triggering LR's "changed by both" conflict dialog with develop-setting loss risk ([Adobe Community](https://community.adobe.com/questions-675/lightroom-classic-and-peakto-metadata-settings-981421)).

### 5. Offline volumes — a headline feature

Yes, and it's central to the value prop. Index a catalog/folder on an external drive, disconnect it, and you can still browse, search, annotate, and organize; previews and metadata persist in Peakto's DB. Deduplication also works on offline sources (with lower-res review previews). This works precisely *because* it stores its own small thumbnails rather than relying on the volume.

### 6. Sync / teams / pricing

There is no "Peakto Sync" multi-Mac replication product. The team story is **Peakto-as-local-server**: 2.5 turns a Mac into an encrypted, peer-to-peer-accessible media server with a browser web app — collaborators search the local AI index, preview video proxies, annotate, comment, set validation statuses, and download originals, with shared spaces, role permissions, time-limited guest links, and a change log. No cloud upload. Positioned as local Frame.io. ([2.5 press release](https://cyme.io/en/press/peakto-eliminates-cloud-dependence/))

Pricing: Standard from ~$10/mo (2-yr) / $12 annual / $15 monthly; Standard perpetual ~$189 with 1 year of updates; Professional from $25/mo per user (adds collaboration, guest sharing, Premiere plugin); Enterprise custom. Peakto Search (LR/C1 plugin only) $8.99/mo or $129 lifetime. 7-day trial; **no trial on the lifetime license**. ([pricing](https://cyme.io/en/pricing/), [PetaPixel](https://petapixel.com/2025/10/08/photo-management-app-peakto-app-now-has-secure-online-collaboration/))

### 7. Praise vs. complaints

Praised: seeing 350k photos from Aperture + Lightroom + C1 in one place; resurrecting dead-app libraries; recovering face annotations from Aperture into a C1 world; compact database (~3 GB per 100k images, far smaller than NeoFinder/Photo Mechanic per one user); offline drive search; responsive dev team; NAB 2025 Product of the Year.

Complaints: **ingestion is slow and resource-hungry** (3 hours for external drives; one 2023 reviewer: 90 min for 2.5 GB of LR catalogs, 8 GB consumed); low-res previews on high-DPI displays; EXIF/date fields not always read correctly from source catalogs; **aesthetic/technical scores poorly correlated with human taste** (raised independently by at least three reviewers); AI struggles on non-conventional work (film scans with sprocket holes, databending); grid-view scroll performance; UI niggles (keyword editor focus bugs, no result count, video playback controls); no negative search operators; onboarding confusion; no DaVinci Resolve *catalog* ingest yet (export only, on roadmap). ([Phoblographer](https://www.thephoblographer.com/2023/04/16/still-a-long-way-from-being-great-peakto-review/), [Scroogie Boy](https://blog.scroogieboy.com/2025/01/12/evaluating-peakto-for-picture-library.html), [Shotkit](https://shotkit.com/peakto-review/))

### 8. Design lessons for a Rust local-first competitor

**Worth borrowing:**
- Read-only native parsers for foreign catalog formats, decoupled from the vendor app being installed. This is the moat and it's pure file-format work — well suited to Rust.
- Own small thumbnails + lazy high-res fetch from the source's existing previews. Gives offline-volume browsing nearly for free and keeps DB size ~30 KB/image achievable.
- Offline-volume search as a first-class guarantee, not a degraded mode.
- Local-server-plus-browser instead of cloud sync. Sidesteps the entire storage-cost and privacy argument, and is a natural fit for an axum/tower service.
- Layered search: semantic embedding + speech transcript + hard metadata filters, combined. Users report the value is in the *combination*, not semantic alone.
- Background-work throttle exposed in the menu bar (Benz specifically praised this) — cheap to build, high perceived quality.

**Gaps to attack:**
- **Ingest throughput.** Their weakest, most-complained-about axis; a Rust pipeline with proper parallelism and incremental checkpointing is a defensible differentiator. Support pause/resume.
- **Bidirectional write-back.** XMP roundtrip limited to Watched Folders, no rating/flag sync to Lightroom, and conflict-dialog collisions. A correct, conflict-aware XMP/sidecar strategy (and honest documentation of LR's develop-setting hazard) would win the power users.
- **Aesthetic scoring credibility.** Repeatedly dismissed as not matching user taste. Either make it user-calibrated/learnable per-library, or don't ship a single scalar "quality" score at all.
- **Preview resolution** on Retina/5K — don't cap thumbnails at a 2019 assumption.
- **Non-mainstream imagery** (film scans, experimental, graphic design assets) breaks their classifier taxonomy; a fixed category list is brittle where open-vocabulary embeddings are not.
- **Result counts, negative/boolean query operators, saved queries** — basic search affordances they lack.
- No DaVinci Resolve project ingest; no Windows/Linux; no ASC MHL / checksum-verified ingest provenance at all, which is an adjacent opening if the app is ingest-first.
